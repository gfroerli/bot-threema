# Gfrörli Threema Bot

Read `README.md` for a general overview.

## Resources

- Gfrörli API documentation: https://github.com/gfroerli/api/blob/master/README.md
- To learn about Threema message formatting options, read: https://threema.com/en/faq/markup

## Conventions

### Rust

Imports:

- ALWAYS use merged imports (i.e. NEVER use two separate `use` lines with the same crate next to each other)
- Group imports using the "std / third party / first party (`super::` / `crate::`)" convention
- Don't use `std::*` directly, instead import the corresponding modules or types at the top level
- Don't use `super::*` imports (except in test modules), instead use `crate::` imports

Testing:

- When importing types that are only used for tests, import them inside the `tests` module and do not use `#[cfg(test)]` on top level
- When adding multiple unit tests for a function, struct or enum, wrap them in a dedicated module named after that unit. For example, when a function is called `check_foo`, the test path should be `tests::check_foo::a_test` and `tests::check_foo::another_test`.
- Never use `super::super::(...)` references or imports in tests, always use a `super::*` import in the module (and potentially parent modules).

Logging:

- Use tracing crate for logging
- Start all logs with a capital letter
- Log levels:
  - INFO for primary events that should always be visible in logs
  - DEBUG for more detailed events (DEBUG is enabled by default for this crate)
  - TRACE for low-level events that may help with debugging
  - WARN for things that went wrong, but don't require attention (e.g. handled errors)
  - ERROR for actual errors, i.e. things that may require attention

Other:

- Sort dependencies (in `Cargo.toml`) and imports alphabetically
- Check if code compiles with `cargo check`
- Lint code with `cargo clippy`
- At the end, when everything else works fine, ALWAYS format code with rustfmt through `cargo fmt`
- NEVER use `unsafe` unless it is absolutely required to do so. If you think it is required, ALWAYS ask the developer for permission, along with a rationale.
- Type safety is great, use it to make illegal state unrepresentable and to enforce business logic

### Database

- Use `uid` column name for the primary key
- Add a `uid` field to every SQLite table - it's there in the background anyways
