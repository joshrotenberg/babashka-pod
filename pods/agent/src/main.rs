//! The `pod-agent` binary: serves [`pod_agent::namespace`] over the transport
//! babashka asks for, then cancels any turn still running.

use std::time::Duration;

use babashka_pod::{Format, Pod};

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("pod-agent is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }
    let result = Pod::new()
        .format(Format::TransitJson)
        .namespace(pod_agent::namespace())
        .run();
    // Background turns are not pod calls, so shutdown does not reach them.
    pod_agent::shutdown(Duration::from_secs(2));
    if let Err(e) = result {
        eprintln!("pod-agent: {e}");
        std::process::exit(1);
    }
}
