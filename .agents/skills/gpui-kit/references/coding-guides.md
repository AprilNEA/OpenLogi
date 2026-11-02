---
title: Coding Guides
description: Architecture and coding conventions for maintainable GPUI Kit applications
order: -2.2
---

# Coding Guides

This guide describes the application architecture and code patterns that have
proved durable in GPUI Kit. It is written for both engineers and coding
agents. Read [Design Guides](./design-guides.md) first: code structure should
preserve product intent, not replace it.

This is a normative guide. **Must** marks lifecycle, correctness, or ecosystem
constraints; **should** is the default architecture and requires a concrete
reason to depart from it. Current source and API docs remain authoritative for
exact signatures.

## Architecture at a glance

<img class="architecture-light" src="/application-layers-light.svg?v=20260822-16" alt="GPUI application architecture layers">
<img class="architecture-dark" src="/application-layers-dark.svg?v=20260822-16" alt="GPUI application architecture layers">

Dependencies point downward. Higher layers own domain meaning and orchestration;
lower layers own reusable presentation or behavior. Do not make a reusable
component depend on an application screen, or make `gpui-base` depend on a
theme from GPUI Component.

Use these boundaries:

- **app shell:** compose windows and feature crates while keeping feature logic out;
- **feature crate:** keep one capability's model, services, views, commands, dialogs,
  and workflow behind one public boundary;
- **app component:** a repeated domain-aware pattern;
- **gpui-component:** themed, general-purpose UI;
- **gpui-base:** reusable behavior and geometry without product presentation.

### Organize large applications by capability

In a large Rust application, a feature should usually be a crate, not another
file in a global `views`, `models`, or `modals` directory. Keep the model,
views, commands, dialogs, and workflow for one capability together. A dialog
that edits a workspace belongs to the workspace feature; only the reusable
dialog primitive belongs to the UI library.

```text
crates/
├── app/
│   └── src/main.rs             # Compose windows and features
├── workspace/
│   └── src/
│       ├── lib.rs              # The feature's public boundary
│       ├── model.rs
│       ├── commands.rs
│       ├── workspace_view.rs
│       └── rename_dialog.rs
├── search/
│   └── src/
│       ├── lib.rs
│       ├── model.rs
│       ├── commands.rs
│       ├── search_view.rs
│       └── filters.rs
├── settings/
│   └── src/
│       ├── lib.rs
│       ├── model.rs
│       ├── settings_view.rs
│       └── account_dialog.rs
└── shared/
    └── src/
        ├── lib.rs
        └── recent_items.rs     # A stable capability with multiple owners
```

Do not invert this into global `models/`, `views/`, `modals/`, and `commands/`
directories. Those folders classify files by implementation role while
scattering every feature across the application.

The application shell composes feature crates but contains little feature
logic. A feature may depend on stable shared capabilities and UI foundations;
it must not depend on the shell or reach into a sibling feature's internals.
When two features need to communicate, prefer an explicit command, event, data
type, or small shared service over a dependency between their views. Extract a
shared crate only after the capability has a coherent name and more than one
real owner.

Crate boundaries are engineering boundaries. They let Cargo rebuild and test a
smaller dependency subgraph, make ownership visible in `Cargo.toml`, and limit
the review and regression surface of a change. They also make removal honest:
a feature that cannot be detached without searching through global view and
modal directories was never isolated.

Do not create a crate for every screen or helper. Split where a capability has
its own state and lifecycle, a stable public seam, or enough implementation to
benefit from independent compilation and tests. Keep dependencies acyclic and
pointing toward smaller, more stable crates.

## Bootstrap and root ownership

Initialize GPUI Component once, before creating component-backed views, and put
`Root` at the first level of each window:

```rust
app.run(move |cx| {
    gpui_kit::init(cx);

    cx.spawn(async move |cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let workspace = cx.new(|cx| Workspace::new(window, cx));
            cx.new(|cx| Root::new(workspace, window, cx))
        })
        .expect("failed to open window");
    })
    .detach();
});
```

`Root` coordinates window-level component facilities such as overlays and
notifications. Do not create a separate root for each page inside one window.
It also coordinates modal focus restoration, focus traps, tooltip/menu layers,
and window-scoped text selection. Bypassing it can produce behavior that looks
correct at rest but fails when overlays nest or focus changes quickly.

## Understand GPUI's phases and contexts

GPUI is retained state with declarative rendering. An entity survives across
frames; the element tree returned by `render` is a fresh description of the
current frame. Keep that distinction explicit.

- `Context<Self>` mutates the current entity, creates listeners tied to it,
  emits its events, and notifies its observers.
- `App` gives access to application globals and entity reads/updates without
  implying ownership by the rendered element.
- `Window` owns focus, actions, input dispatch, element-keyed state,
  measurement, and animation-frame requests for that window.
- layout, prepaint, and paint are later phases; use their hooks only when
  resolved geometry is genuinely required.

Never retain `&mut Window`, `&mut App`, or `&mut Context<_>` beyond the call in
which it is provided. Retain typed handles—`Entity`, `WeakEntity`,
`FocusHandle`, scroll handles, or domain IDs—instead.

## Choose the right unit

### Use `RenderOnce` for value-like elements

Use a `RenderOnce`/`IntoElement` component when all inputs can be supplied by
the caller and the element does not need to retain application state between
frames. This is the normal choice for presentational wrappers and small
controls.

```rust
#[derive(IntoElement)]
struct EmptyState {
    title: SharedString,
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .items_center()
            .text_color(cx.theme().muted_foreground)
            .child(self.title)
    }
}
```

### Use `Entity<T>` for retained behavior

Use an entity-backed `Render` view when behavior spans frames or needs
observation, subscriptions, focus, async work, history, measurement, or
incremental updates. Store entities in an owning view rather than recreating
them in `render`.

```rust
struct SearchView {
    query: Entity<InputState>,
}

impl SearchView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search…"));
        Self { query }
    }
}
```

Do not turn every visual fragment into an entity. Entity boundaries have
lifecycle and coordination costs; use them where retained identity matters.

### Elements, views, and behavior systems are different

Do not force every component into one template. The ecosystem contains:

- semantic elements such as Button, Checkbox, Link, and Tabs;
- compound behavior roots such as Dialog, Popover, Select, and Combobox;
- entity-backed systems such as Input, Table, Tree, Dock, and notifications;
- infrastructure such as positioning, virtualization, scrolling, focus traps,
  motion, history, and measurement.

An element may be internally complex and still be value-like to its caller. A
stateful system may expose render callbacks so applications own presentation
without reimplementing behavior. Choose the public seam from the behavior,
not from how many `div`s appear in its renderer.

## State ownership

Put each state in the narrowest owner that can keep it correct:

- domain state belongs to a model or feature view;
- transient view state belongs to the view that renders it;
- reusable behavioral state belongs to the component state designed for it;
- tiny element-local state may use GPUI keyed element state;
- shared application services may be stored as GPUI globals.

Prefer controlled values for ordinary selection and toggles: pass the current
value into the component, receive a requested change, update the owner, and
render again. A callback reports intent; it should not create a second hidden
source of truth.

```rust
Checkbox::new("show-hidden")
    .checked(self.show_hidden)
    .label("Show hidden files")
    .on_click(cx.listener(|this, checked, _, cx| {
        this.show_hidden = *checked;
        cx.notify();
    }))
```

Call `cx.notify()` after a mutation that changes rendering. Use `cx.emit(...)`
for a semantic event that an owner should handle, and `cx.subscribe(...)` or
`cx.observe(...)` when the lifetime should follow an entity. Keep returned
subscriptions alive when the API requires it.

Do not notify merely because a value was read or derived. Avoid unconditional
notification from `render`; it schedules another render and can create a
permanent redraw loop. When several fields form one invariant, update them
together and notify once. A reusable state type that cannot receive a context
should make that limitation explicit and require its owner to emit/notify.

### Avoid state feedback loops

Text input, selection, filters, and controlled popups commonly have two paths:
an external owner updates the value, and user interaction requests a new value.
Do not send an owner-supplied value back through the user callback during sync.
Track the origin or compare coherent snapshots so each logical change is
reported once. Make callbacks re-entrancy-safe when a callback can synchronously
close, replace, or update the component that invoked it.

## Stable identity

An `ElementId` is part of behavior. It gives an element stable identity and keys
element-local or component state. A component may also use it as one input to
its own focus, measurement, or animation identity; focus and scrolling are
otherwise owned by their dedicated handles.

- Use stable domain IDs for rows, tabs, tree nodes, and repeated controls.
- Namespace child IDs with their owning object when the same control repeats.
- Never derive identity from a translated label or a mutable list index when
  items can be inserted or reordered.
- Do not generate a fresh random ID during `render`.

```rust
Button::new(("delete-project", project.id))
    .danger()
    .label("Delete")
```

A changed ID means a changed UI identity. Treat that reset as deliberate.

The same rule applies to transition channels, overlay tokens, scroll handles,
and persistence IDs. If two independently retained behaviors share a key, they
can overwrite each other's state; if one behavior changes keys every frame, it
never accumulates state.

## Rendering and composition

Keep `render` declarative: read current state, derive presentation values, and
compose elements. Move domain operations, parsing, and non-trivial mutation to
named methods or services.

```rust
impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(self.render_content(cx))
    }
}
```

Extract a render helper when it names a meaningful region and reduces the
amount of state a reader must hold at once. Extract a new component when the
region has its own reusable contract or retained lifecycle—not merely because
a builder chain is long.

Use GPUI Component's fluent traits consistently (`Sizable`, `Disableable`,
`Selectable`, and component-specific builders). Prefer `.when(...)` and
`.when_some(...)` for small conditional refinements; use ordinary Rust control
flow when branches represent substantially different interfaces.

Compose from the standard semantic component before building a custom surface.
Do not reproduce a menu, select, dropdown, or command palette from generic
`div`s merely to match one screenshot. Reusing the component preserves its
item geometry, focus transfer, keyboard navigation, selection, disabled state,
dismissal, and accessibility contract. If the standard component cannot
express a recurring valid pattern, improve its explicit API instead of styling
arbitrary descendants at each call site.

Render callbacks supplied by application code should be side-effect-free. A
list item renderer, menu builder, or dock panel renderer may run whenever its
owner needs to measure or redraw. It must not perform a business operation,
append data, or register an unbounded subscription.

## Behavior and presentation boundary

The durable Base rule is:

> Base owns reusable behavior and the geometry required to implement it. The
> presentation layer owns the product's visual language.

“Headless” does not mean “one empty `div`.” Popup collision, keyboard
navigation, editing, virtualization, resize arithmetic, focus trapping, and
dock reconciliation require internal structure and state. Moving that work to
every caller would not create flexibility; it would duplicate fragile
behavior.

Conversely, Base must not choose brand colors, typography, density, final
icons, component variants, or application composition. Expose presentation
through `Styled`, typed semantic-state styles, explicit parts, child slots, and
item renderers. Do not inspect arbitrary descendants to discover titles,
descriptions, or close buttons—make semantic parts explicit.

## Theme and styling

Read semantic values from the active theme and apply layout with GPUI's
`Styled` methods:

```rust
div()
    .bg(cx.theme().background)
    .text_color(cx.theme().foreground)
    .border_1()
    .border_color(cx.theme().border)
    .rounded(cx.theme().radius)
```

Rules:

- do not hard-code product colors, corner radii, spacing, or control geometry;
- application code must not introduce raw hex, `rgb`/`rgba`, or `hsla`; read a
  semantic color from `cx.theme()` or add the missing role to the product theme;
- application layout should use GPUI's rem-based scale helpers (`p_2()`,
  `gap_3()`, `w_64()`, `text_sm()`) instead of direct `px(...)` values;
- use semantic tokens for meaning, not palette position;
- keep state-independent geometry in the ordinary builder chain;
- use GPUI `hover`, `active`, `focus`, and `focus_visible` modifiers for
  runtime interaction states;
- use a component's semantic state styles for checked, selected, pressed, or
  disabled appearance;
- keep popup ownership in explicit state so its trigger can render the open or
  pressed appearance until dismissal;
- guard hover/active refinements when a disabled control must not react;
- keep component variants few and meaningful rather than adding a variant for
  every call site.
- bind primary styling to the decision area's real default commit and Enter
  action; do not derive it from action count, frequency, or toolbar position.
- keep Badge and Alert variants semantic and scarce. Ordinary metadata stays
  neutral; do not map every enum case or section to a different color merely
  because a variant exists.

The effective precedence is instance style, active semantic value states,
disabled state, then GPUI runtime interaction refinements. Later layers only
replace fields they set.

Prefer `Theme::semantic_tokens()` for new application-owned presentation. The
semantic token surface contains generic color roles plus radius, spacing,
typography, and shadow scales; it deliberately avoids component names. Legacy
component-specific theme values still exist for compatibility but should not
become the extension point for every application widget.

There is one current ownership caveat: `Theme::spacing_tokens()` projects the
default scale, and `Theme::apply_semantic_tokens(...)` does not store custom
spacing or elevation scales. An application that customizes those scales must
retain its own `SemanticThemeTokens` (or narrower design-system state) and make
that state available to its components. Do not write a custom spacing snapshot
into the global theme and expect a later `cx.theme().semantic_tokens()` call to
return it.

If code mutates the global GPUI Component theme directly, call
`Theme::sync_base(cx)` afterward so Base-owned scrollbars and resize handles
receive the new projection. `Theme::change(...)` performs this projection as
part of a complete theme change.

An outward focus ring needs physical room. An ancestor with
`overflow_hidden()` clips it. Prefer layouts that leave room; if a product must
clip heavily, use the theme's focus-ring policy and retain the focused border
instead of silently hiding all keyboard focus.

### Base font is the application zoom control

`Root::render` calls `window.set_rem_size(cx.theme().font_size)`. Therefore the
theme's base font is not only body typography; it is the reference length for
the application's rem-based design scale. This deliberately follows the useful
part of Tailwind's model: named type, spacing, and size steps share one relative
base instead of becoming unrelated pixel constants.

Change zoom by updating the base font and refreshing the window:

```rust
Theme::global_mut(cx).font_size = px(18.);
Theme::sync_base(cx);
window.refresh();
```

The base font itself is a pixel value because it anchors the scale. Descendant
application UI should normally use relative helpers—`text_sm()`, `gap_2()`,
`px_3()`, `h_8()`, `size_4()`—so type, whitespace, controls, and icons respond
together. A custom component that combines rem-based text with fixed-pixel
padding or icon geometry must document why that part should not zoom.

Treat every direct `px(...)` and raw color constructor in application UI as a
review finding. Accept it only for a documented physical/platform boundary,
measured runtime geometry, raster/data color, or the theme/token definition
itself. Convenience and matching a screenshot are not valid exceptions.

Anything cached from resolved layout must include `window.rem_size()` in its
invalidation key, directly or through a revision that changes with it. This
includes wrapped row heights, text shaping/layout, virtual-list measurement,
popup and dialog geometry, icon sizing derived from text, and custom canvas
metrics. The Command component's variable-height rows are an ecosystem example:
they remeasure when rem changes because the same fixed width wraps differently
at a larger base font.

Do not confuse this application zoom with Dock panel zoom. Dock zoom is a
stateful layout operation that makes one tab group fill the DockArea
while keeping the container chrome and the way back out. It must not modify the
window rem size.

## Events, actions, and focus

Use pointer callbacks for pointer-specific behavior. Use GPUI Actions for
commands that should support key bindings, menus, or dispatch from multiple
inputs. Keep action handlers close to the view that owns the command.

Model one logical desktop command once. A toolbar Button, `DropdownMenu` item,
`ContextMenu` item, menu-bar item, and key binding should dispatch the same
Action or call the same owner method instead of copying five mutations. Derive
their label, icon, shortcut, and enabled state from one command policy where
practical, so the entry points cannot disagree. The menu owns navigation and
dismissal; the feature owner still owns whether the command is allowed and
what it does.

Preserve semantic roles in the element choice. Use `Button` for commands even
when the desired treatment is quiet—select `outline`, `ghost`, or an icon
presentation instead of replacing it with `Link`. GPUI Kit applications
reserve `Link` for targets opened by a browser or mail client, such as a URL,
web document, or email address. Use the relevant navigation component for an
in-app destination and `Button`/`Action` for a command. This is a product
convention, not a limitation of `gpui_kit::base::Link`, whose `open_with` seam can
route a destination elsewhere.

Only stop propagation when a nested interaction must prevent its parent from
handling the same event. Blanket propagation stops break menus, selection,
dragging, and window-level commands in ways that are difficult to diagnose.

Make focus ownership explicit:

- retain a `FocusHandle` in the entity that owns keyboard interaction;
- register key contexts and actions on the appropriate focused region;
- transfer focus when opening an overlay and restore it on dismissal;
- render a visible `focus_visible` state;
- do not request focus unconditionally from `render`.

Attach a `key_context` and its `on_action` handlers to the same focused region.
Bindings are contextual: a registered Action without the intended focus path
is not a working keyboard interaction. Composite widgets should implement the
complete navigation model—arrow movement, Home/End or page movement where
appropriate, confirmation, cancellation, and Tab behavior—rather than a few
isolated shortcuts.

Modal surfaces must trap focus and restore the previous valid focus target on
dismissal. Nested overlays dismiss from the top. Handle rapid close/open
sequences without restoring focus through an intermediate, already-closing
surface.

## Async work and side effects

Start async work from an event, lifecycle hook, or named method—not as an
unconditional side effect of `render`. Capture weak entities when work should
not keep a closed view alive. When the task completes, update state through the
GPUI context, handle the case where the entity or window no longer exists, and
notify once after the coherent state change.

Represent async operations with explicit states such as idle, loading, loaded,
and failed. Preserve usable previous data during refresh when possible. Prevent
duplicate destructive submissions and surface recoverable errors in the UI;
do not rely on logs as user feedback.

Use background executors for expensive parsing or computation, but keep GPUI
entity mutation on the appropriate application context. Results can arrive
after the request, document, view, or selection has changed; attach a revision
or identity and reject stale work rather than applying it to new state.

## Layout, measurement, and scrolling

`h_flex` centres its children on the cross axis; `v_flex` leaves flexbox's
default, `stretch`. This matches Zed's `h_flex`, and it is what a row of
controls wants, so a row of icon and label says nothing. It is not what a row
of full-height columns wants: a column placed in a bare `h_flex` does not fill
the row's height, so a column taller than the row is centred and its top —
commonly a header — is clipped off the top of the window, with nothing near the
column to say why. A row whose children are columns says `items_stretch()`:

```rust
h_flex()
    .items_stretch()
    .size_full()
    .child(sidebar)
    .child(content)
```

Most UI should use GPUI layout rather than measuring itself. Measurement is a
deep behavior tool for popups, virtualization, editors, resize handles, charts,
and similar components whose correctness depends on resolved geometry.

- Put measurement and geometry in the layer that owns the behavior.
- Observe bounds in prepaint only when ordinary layout cannot express the
  relationship.
- Never mutate unrelated application state every prepaint.
- Treat measured data as frame- or revision-scoped; it can become stale after
  typography, rem size, width, theme, or content changes.
- Centralize shared geometry such as popup flipping and viewport clamping so
  every overlay follows the same edge policy.

For alignment invariants, prefer construction over correction: sibling regions
should consume the same spacing token or shared inset instead of repeating
equivalent literals. Add geometry assertions or visual regression coverage for
critical repeated edges, columns, and gaps. Exercise more than the default
window: rem zoom and display scaling can turn fractional coordinates into a
one-physical-pixel drift even when the default screenshot looks aligned.

Measure the resolved result when reviewing precision, but do not encode a
measured correction as a raw `px(...)` nudge. Trace the mismatch to duplicated
padding, nested insets, border ownership, font metrics, or rounding, then fix
the structural owner.

`h_flex()` and `v_flex()` are not mirror images: `h_flex()` centers its
children on the cross axis, `v_flex()` leaves them stretching. A column placed
in a row therefore takes its content's height, not the row's, and a column
taller than the row is centered — its header is pushed off the top edge and
clipped. Give a full-height column `h_full()`, or give the row `items_start()`
or `items_stretch()`, whenever the child owns a header, a footer, or a scroll
region that must resolve against the row's height.

Every scrollable region must have one owner. In flex layouts, apply
`min_w_0()` or `min_h_0()` to the flexible child that is allowed to shrink.
A flex item only drops its content-based automatic minimum size when its own
overflow is not visible, so an ordinary flexible child refuses to shrink around
long content until you say so. `Scrollable` handles this for its own wrapper,
but a plain `div` between it and the flex container still needs the minimum
released. Avoid accidental nested scrolling; route wheel input to the intended
axis and preserve platform/wasm differences when an API is not portable.

Attach `Scrollable` to the element that owns the full panel, editor, or window
viewport so its scrollbar resolves against the region edge. Put content inset
inside that scroll owner rather than wrapping the scroll owner in a padded
container. A scrollbar floating between content and the panel boundary usually
reveals the wrong scroll owner or padding on the wrong layer.

## Lists, tables, and large data

Use virtualization when data can grow beyond a small, bounded collection. Keep
row identity separate from visible position and avoid cloning the full data set
on every render. Let a stateful list or table own navigation, selection, scroll
coordination, and visible-range calculation while item renderers own row
presentation.

Separate:

- source data and domain IDs;
- filtering/sorting state;
- selection state;
- viewport/scroll state;
- row rendering.

This keeps updates local and prevents the view tree from becoming the data
model.

Virtualization is a behavioral contract, not just a performance switch. Item
measurement must be invalidated when width, typography, rem size, or row
content changes. Keyboard selection and scroll-to-item must operate in model
coordinates even when most elements do not exist in the current frame.

## Public API design

For reusable components:

- constructors should establish valid defaults;
- builders take and return `Self` and use domain language;
- callbacks describe requested changes and include pointer events only when
  modifiers or pointer details are meaningful;
- evolvable behavioral seams use private fields, builders for construction,
  and readers for inspection;
- boolean readers use `is_` or `has_` where a same-named builder exists;
- non-boolean setters use `with_` when readers need the plain field name;
- explicit compound parts are preferable to inspecting arbitrary descendants;
- adding reusable behavior must not force a product-level visual choice.

Private fields are the default for behavioral state that must evolve without
breaking callers. Public fields are appropriate for deliberately record-like
configuration, theme tokens, geometry, and serialized schemas. Every public
struct with public fields must carry `#[non_exhaustive]`. Provide constructors,
`Default`, or builders so callers can create values without exhaustive struct
literals. This preserves the ability to add fields without breaking callers.
Apply this rule to new types and public API changes; unrelated existing types
can be migrated separately.

Keep public module paths stable while reorganizing internals: use a module seam
with deliberate re-exports so folders can change without forcing downstream
imports to change. Prefer platform control terminology and established project
naming over web-framework vocabulary.

## Platform and capability boundaries

Do not assume every native or web target supports the same facility. Window
decorations, accessibility bridges, system notifications, clipboard behavior,
scroll gestures, fonts, and timing can differ. Put platform-specific code
behind a narrow capability seam and define the fallback behavior.

A platform branch must preserve the semantic contract even if presentation
differs. For example, a system notification may have different retraction
support, but the application still needs a coherent delivery state. Test both
the shared state machine and the platform adapter where possible.

## File and naming conventions

- Name views and entities after product concepts: `ProjectList`,
  `ProjectEditor`, `SettingsState`.
- Name event handlers after intent: `confirm_delete`, `open_project`,
  `on_query_changed`.
- Keep one main responsibility per module; split a file when state ownership or
  lifecycle can no longer be understood without reading unrelated behavior.
- Keep component module, state, events, and focused tests together when they
  change together.
- Document invariants and surprising lifecycle constraints; do not narrate
  obvious builder calls.
- Use `rustfmt` and satisfy the workspace's Clippy rules. Avoid broad `allow`
  attributes that conceal unrelated warnings.

### Vocabulary is part of the API

Use the same word for the same concept across components. Before naming a new
method, search GPUI, `gpui-base`, and GPUI Component for the established term;
prefer macOS/Windows control terminology where the ecosystem has no precedent.
Localized documentation preserves exact API identifiers and established UI
framework terms when translation would reduce precision. Format identifiers as
code, explain retained terms when needed, and do not mix languages merely to
make ordinary prose sound technical.

| Concept | Naming pattern | Example |
| --- | --- | --- |
| Value-like rendered control | noun | `Button`, `Checkbox`, `Tab` |
| Retained behavioral model | `<Control>State` | `InputState`, `TableState` |
| Imperative shared reference | `<Control>Handle` | `DialogHandle`, scroll handle |
| Semantic notification | `<Control>Event` | `TableEvent`, `SelectEvent` |
| Keyboard command | verb or intent noun | `Confirm`, `Cancel`, `SelectNext` |
| Pluggable data/behavior owner | `<Role>Delegate` / `<Role>Provider` | `TableDelegate`, `CompletionProvider` |
| Application-supplied presentation | `render_<part>` or `<part>_renderer` | `render_item` |
| Construction | `new`, or a semantic constructor | `new`, `horizontal`, `vertical` |
| Fluent property | noun/adjective | `label`, `disabled`, `selected`, `placement` |
| General non-boolean replacement builder | `with_<field>` | `with_size`, `with_mode` |
| In-place mutation | `set_<field>` | `set_items`, `set_selected_index` |
| Boolean reader | `is_<adjective>` / `has_<noun>` | `is_open`, `is_closable`, `has_selection` |
| Plain value reader | field noun | `placement`, `selected_value` |
| Callback registration | `on_<event or intent>` | `on_click`, `on_open_change` |
| Rendering a named region | `render_<region>` | `render_toolbar`, `render_content` |

For new APIs, fluent builders omit `set_` because they consume and return
`Self`; mutation through `&mut self` uses `set_`. Preserve established public
names when changing them would cause needless churn. Existing builder names
such as `set_position` are compatibility exceptions, not patterns for new APIs.

A boolean reader is either `has_<noun>`, when the value holds something, or
`is_<adjective>`, when it describes a state or a permission. Reach for the
adjective whenever the action has one: `is_closable` over `can_close`,
`is_zoomable` over `can_zoom`, `is_copyable` over `can_copy`. When the action
is a verb phrase with no adjective form, name the thing it needs instead:
`has_definition`, not `can_go_to_definition`. Do not add new `can_` readers.

Boolean builders may use the field name (`disabled(bool)`) while their readers
use `is_disabled()`. For a public seam struct containing non-boolean fields,
use `with_item_ix(...)` for construction and `item_ix()` for reading so setter
and getter names never collide. Prefer `_ix` for new local or internal
zero-based indices, preserve established public terms such as `selected_index`,
and do not introduce `_idx`. If callers never construct the seam value, do not
publish a builder merely for symmetry.

### Let the enclosing name carry the context

A name is read inside something. A field is read inside its type and a parameter
inside its method, so neither repeats what encloses it: `with_item_ix(ix)`, not
`with_item_ix(item_ix)`.

Keep one type's fields at the same level of abbreviation. A single field spelled
out in full becomes the odd one out, and a reader goes looking for the
distinction that made it different. Because a builder is named `with_<field>`,
shortening a field shortens its builder with it and the pair stays matched.

Shorten only where the enclosing name really does disambiguate. When a short
form is also the established term for a *different* quantity elsewhere in the
ecosystem, say which one you mean in the doc comment rather than lengthening the
identifier — the doc is read at the call, and it can explain what a longer name
could only hint at.

### Use precise domain words

- **selected** is persistent membership or the active item; **focused** is the
  current keyboard target; **hovered** is pointer presence; **confirmed** is an
  activation result. Never use them interchangeably.
- **open/close** describes an overlay or disclosure state; **show/hide** is for
  transient presentation requests; **expand/collapse** describes structure.
- **disabled** prevents interaction; **read-only** permits navigation and
  selection but prevents editing; **loading** prevents duplicate work while an
  operation is pending.
- **index** is a current positional coordinate; **id** is stable identity;
  `IndexPath` represents hierarchical position. Do not persist or key
  reorderable data by index.
- **value** is controlled domain data; **presentation** is a read-only snapshot
  prepared for rendering; **state** is retained behavior.
- **placement** is a side or anchor policy; **position** is resolved geometry.
- **size** is a semantic control tier; **width/height/bounds** are geometry.
- **child/children** follows GPUI composition; named slots such as `header`,
  `footer`, `trigger`, and `content` carry additional semantics.

Avoid vague public names such as `data`, `item2`, `handle_action`, `update_ui`,
`process`, `manager`, or `config` when a narrower domain term exists. `Manager`
is appropriate only when a type truly coordinates a collection or lifecycle,
as `ToastManager` does.

### Type and module style

- Rust types and Actions use `UpperCamelCase`; modules, functions, methods,
  fields, and local variables use `snake_case`; constants use
  `SCREAMING_SNAKE_CASE`.
- A module named after a component owns its public seam. Internal folders may
  split state, element, geometry, platform adapter, and tests without leaking
  those folder names into imports.
- Use singular module names for one component concept and established
  ecosystem names for families (`input`, `table`, `dock`).
- Suffix type-erased wrappers with `Any` only when they erase a real type
  boundary, such as `AnyInputState` or `AnyElement`.
- Suffix identifiers with `Id`, zero-based indices with `ix`, and collections
  with meaningful plurals. Do not alternate `idx`, `index`, and `ix` in one
  subsystem.
- Name predicates positively when possible. A positive `enabled`/`visible`
  contract is easier to compose than multiple negatives, but preserve
  established API terms such as `disabled` where they match control semantics.

### Callback and event wording

Use `on_click` only for a genuine click-level contract. A controlled semantic
primitive in Base should prefer `on_change(next_value, ...)`; a styled
compatibility component may retain `on_click` when pointer details or existing
API expectations matter. Do not invent a `ClickEvent` for a model-driven
change.

Name before/after lifecycle hooks precisely. `on_will_change` can veto or
prepare; `on_change` observes a requested/current value contract; `on_confirm`
commits a choice; `on_dismiss` closes a transient surface. Document whether a
callback runs before internal state changes, after them, or instead of them,
and whether it may synchronously re-enter the component.

### Documentation and copy style

Public docs should begin with what a type does and who owns its state. Examples
must use current, compilable APIs and show stable IDs. Document defaults,
platform limitations, focus behavior, callback ordering, and any requirement
to call `notify`, `emit`, or a theme synchronization method.

Follow the [interface-language rules](./design-guides.md#interface-language) for
labels, commands, confirmation dialogs, capitalization, and ellipses. Keep one
canonical term for each domain object, command, and state.

Translation keys describe stable intent (`dialog.delete_project.title`), not a
source-language sentence or a screen coordinate. Never assemble a sentence
from translated fragments or reuse one key for meanings that happen to share
the same English text.

Localize intent, not syntax. Give every locale control over word order,
pluralization, punctuation, and the amount of context it needs. Review strings
inside the component and with realistic data. Tests or linting should catch
missing keys, unintended CJK text in English resources, three-dot ellipses,
unreviewed ALL CAPS, and inconsistent fixed terms; human review still decides
whether repetition is justified by context. Verify every string inside its
component with realistic content, text expansion, and application zoom.

## Testing strategy

Test at the lowest layer that can prove the behavior:

1. pure tests for state transitions, geometry, parsing, and ordering;
2. GPUI context tests for entities, events, and subscriptions;
3. `VisualTestContext` interaction tests for focus, keyboard, pointer, layout,
   and rendered state;
4. example or application smoke tests for complete workflows.

For an interactive component, cover the semantic contract rather than its
implementation details: pointer and keyboard activation, controlled value
changes, disabled behavior, focus movement, event count/order, stable identity,
and important empty or failure states. Add a regression test before fixing a
bug whenever the failure can be reproduced deterministically.

For UI behavior that depends on the real window system, test through the
accessibility tree by role, label, value, enabled state, focus, and selection.
Re-read the tree after every state-changing action because element indexes are
snapshots. Use screenshots for visual facts the semantic tree cannot express;
use coordinate input only as a fallback. Report automated and manual evidence
separately.

## Performance rules

- Do not mutate state or notify unconditionally in `render`.
- Avoid rebuilding entities, subscriptions, focus handles, and expensive data
  structures per frame.
- Notify the narrowest owning entity after a coherent state change.
- Virtualize long collections and render only the visible range.
- Avoid cloning large strings or collections solely to satisfy a closure;
  capture stable handles or shared data.
- Measure before adding caches. A cache must have a clear invalidation owner.
- Keep animation work bounded and honor reduced motion.

## Common failure modes

Avoid these patterns:

- one entity containing the entire application's unrelated state;
- business logic and network requests embedded in a long `render` method;
- random or index-based `ElementId` values for reorderable content;
- literal colors and radii that break custom themes;
- custom clickable `div`s where a semantic component already supplies focus,
  keyboard, disabled, and accessibility behavior;
- duplicated local state that drifts from a controlled model value;
- `cx.notify()` loops caused by mutation during every render;
- nested scroll containers without explicit ownership;
- a new component variant for a one-off screen;
- confirmation dialogs for reversible, low-risk actions;
- tests that call internal methods but never exercise keyboard or pointer
  behavior.

## Rules for coding agents

Before editing, an agent must read the nearest implementation, its tests, the
re-export seam, and the relevant component documentation. It must search the
current source for signatures instead of translating a React, CSS, or old GPUI
example by analogy.

For each change, the agent should be able to name:

1. the behavior owner and presentation owner;
2. the retained identity and state lifecycle;
3. the pointer, keyboard, focus, and accessibility contract;
4. the layout and overflow owner;
5. the theme tokens and intentional exceptions;
6. the test that would fail if the behavior regressed.

Generated code must be reviewed and tested by a person. “Compiles” is not a UI
quality bar, and a broad refactor that merely makes generated code look tidy is
not a substitute for matching the repository's architecture.

## Implementation checklist

Before opening a change for review, confirm that:

- state and side-effect ownership are explicit;
- `RenderOnce` versus `Entity<T>` is chosen deliberately;
- repeated elements have stable domain-based IDs;
- theme tokens and component sizes replace isolated visual literals;
- keyboard actions, focus, disabled state, and overlays work together;
- loading, empty, error, and cancellation paths are represented;
- long data sets use an appropriate virtualized component;
- public API additions preserve dependency direction and encapsulation;
- tests prove behavior at the appropriate layer;
- formatting, Clippy, targeted tests, and relevant examples pass.

See [Getting Started](./getting-started.md) for application setup and the
component pages for current API details.
