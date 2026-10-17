# Component-family conventions

Use this page to predict a component's shape before reading its detailed API. These conventions describe Rust GPUI Component. JavaScript uses its host's generated catalog; a Rust method is not automatically a JavaScript method.

## Choose the family

| Task | Family and constructor | State owner | Event or composition |
| --- | --- | --- | --- |
| Execute a command | `Button::new("save")` | Application owns the operation | `on_click` receives a click event |
| Change a boolean | `Checkbox::new("remember")`, `Switch::new("enabled")`, `Radio::new("choice")` | Application supplies `checked(bool)` | `on_change` receives the requested `&bool` |
| Choose one radio option | `RadioGroup::new("delivery")` | Application supplies `selected_index(Option<usize>)` | `on_change` receives the requested `&usize` |
| Edit retained text | `Input::new(&self.input)` | View retains `Entity<InputState>` | Retain the subscription; handle `InputEvent` |
| Lay out fields | `Form::new()` | Children retain their own state | `child(Field)`, `columns`, `label_layout`, `footer` |
| Supply a compound part | `Field::new()`, `Tab::new()` | Containing component owns placement/selection | Read the parent's accepted child type |

Constructors establish the component family's ownership model. Do not infer that every `new` takes an ID or that every `child` accepts an arbitrary element. Keep domain IDs stable when controls are reordered. Positional selection APIs remain positional; applications map them to domain identity when needed.

## A value request is different from a command

For Checkbox, Switch, Radio, and RadioGroup, use the same owner-update pattern:

```rust
Switch::new("enabled")
    .checked(self.enabled)
    .on_change(cx.listener(|this, next, _, cx| {
        this.enabled = *next;
        cx.notify();
    }))
```

Replace Switch with Checkbox or Radio without changing the boolean callback shape. Radio represents selecting an option; its activation semantics still differ from toggling a Switch. RadioGroup supplies an index, so its owner stores `Some(*next)` instead.

`on_change` requests a value; it does not mutate your application model. Existing `on_click` calls on these four controls remain valid compatibility aliases. Both names set one handler, and the last call wins. Use `Button::on_click` for commands. Retained controls such as Input keep their existing event/subscription API because the state entity, rather than a transient builder, owns their changes.

## Form layout has independent decisions

| Decision | API | Default |
| --- | --- | --- |
| Label above or beside its control | `label_layout(Axis::Vertical / Horizontal)` | Above |
| Number of field columns | `columns(count)` | One |
| A field spanning several columns | `Field::col_span(count)` | One |
| Commands after the fields | `footer(element)` | No footer; supplied footer spans all columns and aligns to the trailing edge |

`Form::horizontal()` and `h_form()` mean labels beside controls. They do not put all fields into one horizontal row. `Form::vertical()`, `v_form()`, and `layout(Axis)` remain supported.

```rust
Form::new()
    .label_layout(Axis::Horizontal)
    .columns(2)
    .child(Field::new().label("Name").child(Input::new(&self.name)))
    .child(Field::new().label("Email").child(Input::new(&self.email)))
    .footer(Button::new("save").label("Save"))
```

The footer supplies layout only: attach the save action to the Button. Form does not own submission, validation, responsive breakpoints, or data persistence. Choose column count for the available width. Put multiple action buttons in one supplied action row with a shared gap. Keep field descriptions with their Field so they follow its alignment.

## Import capabilities explicitly

Components live in their component modules. Common capabilities may be extension traits, not inherent methods: import `ButtonVariants` for button variants, `Sizable` for size tiers, `Disableable` for supported disabled builders, `ActiveTheme` for theme access, and `WindowExt` for window overlays. Do not infer that every component supports every trait.

Use the [complete application recipe](recipes.md) for imports, retained ownership, initialization, Root, and overlay layers. For a narrow task, load its component page and the relevant section of the coding/design guides; a component catalog is not a required reading sequence.

## Verify the convention transfer

When transferring a pattern to another family member, check that it compiles, receives the expected requested value, preserves controlled state after redraw, and behaves correctly when disabled. For Form, check label orientation independently of column count and keep footer geometry outside the field grid's individual cells.

In this repository, `script/check-ai rust` compiles the external-style consumer and runs its interaction tests, control-family event and compatibility tests, and Form geometry tests. These are deterministic contract checks, not measured AI first-attempt success rates.
