<!-- OpenLogi modification: normalized trailing whitespace and final newlines. -->

# Global State

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Use Cases](#common-use-cases) · [Best Practices](#best-practices) · [When to Use](#when-to-use)

## Overview

Global state in GPUI provides app-wide shared data accessible from any context.

**Key Trait**: `Global` - Implement on types to make them globally accessible

## Quick Start

### Define Global State

```rust
use gpui_kit::Global;

#[derive(Clone)]
struct AppSettings {
    theme: Theme,
    language: String,
}

impl Global for AppSettings {}
```

### Set and Access Globals

```rust
fn main() {
    let app = Application::new();
    app.run(|cx: &mut App| {
        // Set global
        cx.set_global(AppSettings {
            theme: Theme::Dark,
            language: "en".to_string(),
        });

        // Access global (read-only)
        let settings = cx.global::<AppSettings>();
        println!("Theme: {:?}", settings.theme);
    });
}
```

### Update Globals

```rust
impl MyComponent {
    fn change_theme(&mut self, new_theme: Theme, cx: &mut Context<Self>) {
        cx.update_global::<AppSettings, _>(|settings, cx| {
            settings.theme = new_theme;
            // Global updates don't trigger automatic notifications
            // Manually notify components that care
        });

        cx.notify(); // Re-render this component
    }
}
```

## Common Use Cases

### 1. App Configuration

```rust
#[derive(Clone)]
struct AppConfig {
    api_endpoint: String,
    max_retries: u32,
    timeout: Duration,
}

impl Global for AppConfig {}

// Set once at startup
cx.set_global(AppConfig {
    api_endpoint: "https://api.example.com".to_string(),
    max_retries: 3,
    timeout: Duration::from_secs(30),
});

// Access anywhere
let config = cx.global::<AppConfig>();
```

### 2. Feature Flags

```rust
#[derive(Clone)]
struct FeatureFlags {
    enable_beta_features: bool,
    enable_analytics: bool,
}

impl Global for FeatureFlags {}

impl MyComponent {
    fn render_beta_feature(&self, cx: &App) -> Option<impl IntoElement> {
        let flags = cx.global::<FeatureFlags>();

        if flags.enable_beta_features {
            Some(div().child("Beta feature"))
        } else {
            None
        }
    }
}
```

### 3. Shared Services

```rust
#[derive(Clone)]
struct ServiceRegistry {
    http_client: Arc<HttpClient>,
    logger: Arc<Logger>,
}

impl Global for ServiceRegistry {}

impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        let registry = cx.global::<ServiceRegistry>();
        let client = registry.http_client.clone();

        cx.spawn(async move |cx| {
            let data = client.get("api/data").await?;
            // Process data...
            Ok::<_, anyhow::Error>(())
        }).detach();
    }
}
```

## Best Practices

### ✅ Use Arc for Shared Resources

```rust
#[derive(Clone)]
struct GlobalState {
    database: Arc<Database>,  // Cheap to clone
    cache: Arc<RwLock<Cache>>,
}

impl Global for GlobalState {}
```

### ✅ Immutable by Default

Globals are read-only by default. Use interior mutability when needed:

```rust
#[derive(Clone)]
struct Counter {
    count: Arc<AtomicUsize>,
}

impl Global for Counter {}

impl Counter {
    fn increment(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    fn get(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}
```

### ❌ Don't: Overuse Globals

```rust
// ❌ Bad: Too many globals
cx.set_global(UserState { ... });
cx.set_global(CartState { ... });
cx.set_global(CheckoutState { ... });

// ✅ Good: Use entities for component state
let user_entity = cx.new(|_| UserState { ... });
```

## When to Use

**Use Globals for:**
- App-wide configuration
- Feature flags
- Shared services (HTTP client, logger)
- Read-only reference data

**Use Entities for:**
- Component-specific state
- State that changes frequently
- State that needs notifications
