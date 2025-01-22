use tracing::{field::FieldSet, Event, Level, Metadata, Subscriber};
use tracing_core::Kind;
use tracing_subscriber::{
    fmt::{format::Writer, FmtContext, FormatEvent, FormatFields},
    registry::LookupSpan,
};

pub struct EventFormatter<const VISITOR_SIZE: usize, F, T> {
    formatter: F,
    check: T,
}

impl<const VISITOR_SIZE: usize, F, T> EventFormatter<VISITOR_SIZE, F, T>
where
    T: Fn(&Metadata<'static>) -> Option<Level> + Send + Sync,
{
    pub fn new(formatter: F, check: T) -> Self {
        Self { formatter, check }
    }
}

impl<const VISITOR_SIZE: usize, F, T, S, N> FormatEvent<S, N> for EventFormatter<VISITOR_SIZE, F, T>
where
    F: FormatEvent<S, N>,
    T: Fn(&Metadata<'static>) -> Option<Level> + Send + Sync,
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();

        if let Some(level) = (self.check)(metadata) {
            let kind = if metadata.is_event() {
                Kind::EVENT
            } else if metadata.is_span() {
                Kind::SPAN
            } else {
                unreachable!()
            };

            let fields = metadata.fields();
            // Safety: at the moment of writing this code, FieldSet is made like
            // ```rust
            // pub struct FieldSet {
            //   names: &'static [&'static str],
            //   callsite: callsite::Identifier,
            // }
            // ```
            // and Identifier is make like
            // ```rust
            // #[derive(Clone)]
            // pub struct Identifier(
            //   #[doc(hidden)]
            //   pub &'static dyn Callsite,
            // );
            // ```
            // that means we can copy the static references without causing any UB
            let cloned = unsafe { std::mem::transmute_copy::<FieldSet, FieldSet>(fields) };

            // here we are leaking memory, but should be mainly references
            let metadata = Box::leak::<'static>(Box::new(Metadata::new(
                metadata.name(),
                metadata.target(),
                level,
                metadata.file(),
                metadata.line(),
                metadata.module_path(),
                cloned,
                kind,
            )));

            let mut visitor = visitor::Visitor::<VISITOR_SIZE>::new();
            event.record(&mut visitor);
            let values = visitor.get_values();
            let valueset = fields.value_set(&values);
            let event = if let Some(parent) = event.parent() {
                Event::new_child_of(parent, metadata, &valueset)
            } else {
                Event::new(metadata, &valueset)
            };
            let res = self.formatter.format_event(ctx, writer, &event);

            // here we're freeing the leaked memory
            // Miri tells us we're doing an invalid operation, because metadata is borrowed for 'static
            // and we don't have any guarantee the implementor of the trait is keeping references to it
            // that is possible, but unlikely.
            // If you're experiencing UB, please enable `i_really_want_memory_leak`  feature
            #[cfg(not(feature = "i_really_want_memory_leak"))]
            drop(unsafe { Box::from_raw(metadata as *const Metadata as *mut Metadata) });

            res
        } else {
            self.formatter.format_event(ctx, writer, event)
        }
    }
}

mod visitor {
    use std::fmt::Debug;

    use tracing::{field::Visit, Level, Metadata, Value};
    use tracing_core::{metadata, Callsite, Field, Interest, Kind};

    const FAKE_FIELD_NAME: &str = "foo";

    // tracing automatically filters out fields with a different call site
    struct FakeCallSite();
    static FAKE_CALLSITE: FakeCallSite = FakeCallSite();
    static FAKE_META: Metadata<'static> = metadata! {
        name: "",
        target: module_path!(),
        level: Level::INFO,
        fields: &[FAKE_FIELD_NAME],
        callsite: &FAKE_CALLSITE,
        kind: Kind::SPAN,
    };

    impl Callsite for FakeCallSite {
        fn set_interest(&self, _: Interest) {
            unimplemented!()
        }

        fn metadata(&self) -> &Metadata<'_> {
            &FAKE_META
        }
    }

    pub struct Visitor<const N: usize> {
        index: usize,
        // TODO: avoid allocating with String
        values: [(Field, Option<String>); N],
    }

    impl<const N: usize> Visitor<N> {
        pub fn new() -> Self {
            Visitor {
                index: 0,
                values: [(); N].map(|_| (FAKE_META.fields().field(FAKE_FIELD_NAME).unwrap(), None)),
            }
        }

        pub fn get_values(&self) -> [(&Field, Option<&dyn Value>); N] {
            let mut index = 0;
            [(); N].map(|_| {
                let val = (
                    &self.values[index].0,
                    self.values[index].1.as_ref().map(|s| s as &dyn Value),
                );
                index += 1;
                val
            })
        }
    }

    impl<const N: usize> Visit for Visitor<N> {
        fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
            // Safety: same assumptions as before, becuase Field is like
            // ```rust
            // #[derive(Debug)]
            // pub struct Field {
            //     i: usize,
            //     fields: FieldSet,
            // }
            // ```
            let cloned = unsafe { std::mem::transmute_copy::<Field, Field>(field) };
            self.values[self.index] = (cloned, Some(format!("{value:?}")));
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing::{Level, Metadata};
    use tracing_subscriber::{
        fmt,
        util::{SubscriberInitExt, TryInitError},
        EnvFilter,
    };

    fn init_tracing(
        check: impl Fn(&Metadata<'static>) -> Option<Level> + Send + Sync + 'static,
    ) -> Result<(), TryInitError> {
        let format = fmt::format()
            .with_target(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .compact();

        // Miri doesn't allow accessing system time
        #[cfg(miri)]
        let format = format.without_time();

        let builder = fmt::Subscriber::builder();

        builder
            .with_env_filter(EnvFilter::from_default_env())
            .event_format(format)
            .map_event_format(|formatter| super::EventFormatter::<10, _, _>::new(formatter, check))
            .finish()
            .try_init()
    }

    #[test]
    fn miri_tracing() {
        init_tracing(|metadata| {
            (dbg!(metadata.file()).is_some_and(|file| file == "src/lib.rs")
                && Level::ERROR.eq(metadata.level()))
            .then_some(Level::WARN)
        })
        .unwrap();

        tracing::error!("test");
    }
}
