# tracing-rewrite

This crate introduces a wrapper that allows conditional rewriting of tracing logs

## Use case

Let's say you are using a third party crate that emits way too many `ERROR` logs, you don't want to suppress them because, well, suppressing errors is never a good idea, but maybe you have your own retry mechanism and your telemetry sistem is configured to raise an alarm with any error or with 10 warnings in a 5 minutes window.
