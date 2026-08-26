//! The nix privileged helper.
//!
//! Runs as root, authorised by polkit, and accepts a **closed set** of typed operations over a
//! socket. It never accepts a free-form command or argument vector: the operation enum is the
//! security boundary, and every addition to it is reviewed as its own small diff.
//!
//! Not implemented yet — this is task 0.9 (`FND-4`). The binary exists now so that the workspace
//! layout, the lint gates and the packaging story are settled before the security-sensitive code
//! is written, and so `cargo build --workspace` covers it from the first commit.

fn main() -> std::process::ExitCode {
    eprintln!(
        "nix-helper {} is not implemented yet (task 0.9). It is not callable and does nothing.",
        nix_core::VERSION
    );
    std::process::ExitCode::FAILURE
}
