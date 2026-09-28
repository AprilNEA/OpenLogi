<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Test Examples

**Contents:** [Testing Best Practices](#testing-best-practices) · [Test Organization](#test-organization) · [Setup and Teardown](#setup-and-teardown) · [Assertions](#assertions) · [Performance Testing](#performance-testing) · [Running Tests](#running-tests) · [Basic Test Execution](#basic-test-execution) · [Test Configuration](#test-configuration) · [Advanced Test Execution](#advanced-test-execution) · [CI/CD Integration](#cicd-integration)

## Testing Best Practices

### Test Organization

Group related tests in modules:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    mod entity_tests {
        use super::*;

        #[gpui_kit::test]
        fn test_creation() { /* ... */ }

        #[gpui_kit::test]
        fn test_updates() { /* ... */ }
    }

    mod async_tests {
        use super::*;

        #[gpui_kit::test]
        async fn test_async_ops() { /* ... */ }
    }

    mod distributed_tests {
        use super::*;

        #[gpui_kit::test]
        fn test_multi_app() { /* ... */ }
    }
}
```

### Setup and Teardown

Use helper functions for common setup:

```rust
fn create_test_counter(cx: &mut TestAppContext) -> Entity<Counter> {
    cx.new(|cx| Counter::new(cx))
}

#[gpui_kit::test]
fn test_counter_operations(cx: &mut TestAppContext) {
    let counter = create_test_counter(cx);

    // Test operations
}
```

### Assertions

Use descriptive assertions:

```rust
#[gpui_kit::test]
fn test_counter_bounds(cx: &mut TestAppContext) {
    let counter = create_test_counter(cx);

    // Test upper bound
    for _ in 0..100 {
        counter.update(cx, |counter, cx| {
            counter.increment(cx);
        });
    }

    let count = counter.read_with(cx, |counter, _| counter.count);
    assert!(count <= 100, "Counter should not exceed maximum");

    // Test lower bound
    for _ in 0..200 {
        counter.update(cx, |counter, cx| {
            counter.decrement(cx);
        });
    }

    let count = counter.read_with(cx, |counter, _| counter.count);
    assert!(count >= 0, "Counter should not go below minimum");
}
```

### Performance Testing

Test performance characteristics:

```rust
#[gpui_kit::test]
fn test_operation_performance(cx: &mut TestAppContext) {
    let component = cx.new(|cx| MyComponent::new(cx));

    let start = std::time::Instant::now();

    // Perform many operations
    for i in 0..1000 {
        component.update(cx, |comp, cx| {
            comp.perform_operation(i, cx);
        });
    }

    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(100), "Operations should complete quickly");
}
```

## Running Tests

### Basic Test Execution

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_counter_operations

# Run tests in a specific module
cargo test entity_tests::

# Run with output
cargo test -- --nocapture
```

### Test Configuration

Enable test-support feature for GPUI tests:

```toml
[features]
test-support = ["gpui/test-support"]
```

```bash
cargo test --features test-support
```

### Advanced Test Execution

```bash
# Run tests with iterations for property testing
cargo test -- --test-threads=1

# Run tests matching a pattern
cargo test test_async

# Run tests with backtrace on failure
RUST_BACKTRACE=1 cargo test
```

### CI/CD Integration

For continuous integration:

```yaml
# .github/workflows/test.yml
name: Tests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: dtolnay/rust-toolchain@stable
      - name: Run tests
        run: cargo test --features test-support
```

GPUI's testing framework provides deterministic, fast, and comprehensive testing capabilities that mirror real application behavior while providing the control needed for thorough testing of complex UI and async scenarios.
