//! The `pod-fswatcher` binary: serves [`pod_fswatcher::namespace`] over the
//! transport babashka asks for.

use babashka_pod::{Format, Pod};

fn main() {
    if !babashka_pod::is_pod_mode() {
        eprintln!("pod-fswatcher is a babashka pod; load it with babashka.pods/load-pod");
        std::process::exit(2);
    }
    if let Err(e) = Pod::new()
        .format(Format::TransitJson)
        .namespace(pod_fswatcher::namespace())
        .run()
    {
        eprintln!("pod-fswatcher: {e}");
        std::process::exit(1);
    }
}
