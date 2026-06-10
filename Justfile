# List available commands
_default:
    @just --list

# run the performance test for a simple build, comparing the standard docker engine with gvisor
run-engine-performance-tests:
    cargo test --test tests compare_default_and_runsc -- --ignored --nocapture
