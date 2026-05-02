// Red-team test binary. Each `rtN` submodule documents one round of
// architectural problems found in the compiler, with one test per
// problem that demonstrates the bug. These tests are *expected to
// fail* (or to surface a misleading error) — they exist to keep the
// problems visible until they're fixed.

#[path = "redteaming/mod.rs"]
mod redteaming;
