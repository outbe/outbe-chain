//! Nested sub-call precompile dispatch shares the live EVM's
//! [`super::OutbePrecompileProvider`] directly, eliminating the old borrow-mode
//! wrapper and `unsafe` pointer casts.
