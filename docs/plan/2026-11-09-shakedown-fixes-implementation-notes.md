## Phase 1: Kill the flaky verify test
### Design decisions
- Write the fake binary via a child `install -m 755` from a staged `tool.src` (src/release/tests/install.rs:fake_install_root): the exec'd file is never opened for write by the test process, so a parallel fork cannot inherit a write handle on it.
- The staged source (`tool.src`) is still written in-process; it is never exec'd, so a leaked handle on it is harmless.
### Deviations
- None.
### Tradeoffs
- Stage-then-`install` vs. spawning `sh -c 'cat > ...'` or a retry on ETXTBSY: `install` is the doc's named mechanism and removes the race rather than papering over it.
### Open questions
- None.
