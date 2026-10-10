# Layout & Styling

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Patterns](#common-patterns) · [Styling Methods](#styling-methods) · [h_flex / v_flex](#h_flex--v_flex-helpers) · [Tailwind Shorthands](#tailwind-style-shorthand) · [Overflow & Scroll](#overflow-and-scroll) · [Absolute Positioning](#absolute-positioning) · [Stacking Order](#stacking-order) · [Theme Integration](#theme-integration) · [Conditional Styling](#conditional-styling) · [Text Styling](#text-styling)

## Overview

GPUI provides CSS-like styling with Rust type safety.

**Key Concepts:**

- Flexbox layout system
- Styled trait for chaining styles
- Size units: `px()`, `rems()`, `relative()`
- Colors, borders, shadows

## Quick Start

### Basic Styling

```rust
use gpui_kit::*;

div()
    .w(px(200.))
    .h(px(100.))
    .bg(rgb(0x2196F3))
    .text_color(rgb(0xFFFFFF))
    .rounded(px(8.))
    .p(px(16.))
    .child("Styled content")
```

### Flexbox Layout

```rust
div()
    .flex()
    .flex_row()  // or flex_col() for column
    .gap(px(8.))
    .items_center()
    .justify_between()
    .children([
        div().child("Item 1"),
        div().child("Item 2"),
        div().child("Item 3"),
    ])
```

### Size Units

```rust
div()
    .w(px(200.))           // Pixels
    .h(rems(10.))          // Relative to font size
    .w(relative(0.5))      // 50% of parent
    .min_w(px(100.))
    .max_w(px(400.))
```

## Common Patterns

### Centered Content

```rust
div()
    .flex()
    .items_center()
    .justify_center()
    .size_full()
    .child("Centered")
```

### Card Layout

```rust
div()
    .w(px(300.))
    .bg(cx.theme().surface)
    .rounded(px(8.))
    .shadow_md()
    .p(px(16.))
    .gap(px(12.))
    .flex()
    .flex_col()
    .child(heading())
    .child(content())
```

### Responsive Spacing

```rust
div()
    .p(px(16.))           // Padding all sides
    .px(px(20.))          // Padding horizontal
    .py(px(12.))          // Padding vertical
    .pt(px(8.))           // Padding top
    .gap(px(8.))          // Gap between children
```

## Styling Methods

### Dimensions

```rust
.w(px(200.))              // Width
.h(px(100.))              // Height
.size(px(200.))           // Width and height
.min_w(px(100.))          // Min width
.max_w(px(400.))          // Max width
```

### Colors

```rust
.bg(rgb(0x2196F3))        // Background
.text_color(rgb(0xFFFFFF)) // Text color
.border_color(rgb(0x000000)) // Border color
```

### Borders

```rust
.border(px(1.))           // Border width
.rounded(px(8.))          // Border radius
.rounded_t(px(8.))        // Top corners
.border_color(rgb(0x000000))
```

### Spacing

```rust
.p(px(16.))               // Padding
.m(px(8.))                // Margin
.gap(px(8.))              // Gap between flex children
```

### Flexbox

```rust
.flex()                   // Enable flexbox
.flex_row()               // Row direction
.flex_col()               // Column direction
.items_center()           // Align items center
.justify_between()        // Space between items
.flex_grow_1()              // Grow to fill space
```

## h_flex / v_flex Helpers

`gpui_kit::component` provides shorthand helpers (import from `gpui_kit::component`):

```rust
use gpui_kit::component::{h_flex, v_flex};

// h_flex() = div().flex().flex_row().items_center()
h_flex()
    .gap_2()
    .child(icon)
    .child(label)

// v_flex() = div().flex().flex_col()
v_flex()
    .gap_4()
    .p_4()
    .child(input1)
    .child(input2)
    .child(submit_btn)
```

These are the standard layout primitives in `gpui_kit::component`. Prefer them over raw `div().flex()`.

## Tailwind-style Shorthand

GPUI provides Tailwind-style spacing/sizing shorthands:

```rust
// Spacing (0=0, 1=4px, 2=8px, 3=12px, 4=16px, ...)
.p_2()    // padding: 8px
.px_4()   // padding x: 16px
.py_3()   // padding y: 12px
.m_2()    // margin: 8px
.gap_3()  // gap: 12px

// Size
.size_full()   // width: 100%, height: 100%
.size_4()      // width: 16px, height: 16px
.w_full()      // width: 100%
.h_full()      // height: 100%
.flex_1()      // flex: 1 1 0 (fill remaining space)
.flex_shrink_0() // prevent shrinking
```

## Overflow and Scroll

```rust
div()
    .overflow_hidden()          // clip content
    .overflow_x_hidden()        // clip horizontal
    .overflow_y_scrollbar()     // show scrollbar on y axis
    .overflow_scroll()          // scroll both axes
```

## Absolute Positioning

```rust
div()
    .relative()                 // position: relative (container)
    .child(
        div()
            .absolute()         // position: absolute
            .top_0()
            .right_0()
            .child("badge")
    )

// Inset helpers
div().absolute().inset_0()      // top/right/bottom/left: 0 (fill parent)
div().absolute().top(px(8.)).left(px(8.))
```

## Stacking Order

```rust
div()
    .relative()
    .child(content)
    .child(
        div()
            .absolute()
            .top_0()
            .right_0()
            .child("badge")
    ) // later children are typically painted above earlier siblings
```

GPUI's general `Styled` API does **not** provide a `z_index(...)` method.

For normal elements, stacking is usually controlled by:

- Parent/child composition
- Absolute positioning
- Render order of siblings (later siblings paint above earlier ones)

If you see a `z_index(...)` method in this repository, make sure it belongs to the specific component you are using: a component's own ordering API is not a general GPUI `Div` styling method.

## Theme Integration

```rust
div()
    .bg(cx.theme().surface)
    .text_color(cx.theme().foreground)
    .border_color(cx.theme().border)
    .when(is_hovered, |el| {
        el.bg(cx.theme().hover)
    })
```

## Conditional Styling

```rust
use gpui_kit::prelude::FluentBuilder as _;

div()
    .when(is_active, |el| el.bg(cx.theme().primary))
    .when(!is_active, |el| el.opacity(0.5))
    .when_some(optional_color.as_ref(), |el, color| el.bg(*color))
```

## Text Styling

```rust
div()
    .text_sm()          // font-size: small
    .text_base()        // font-size: base
    .text_lg()          // font-size: large
    .font_bold()        // font-weight: bold
    .line_height_snug() // tighter line height
    .truncate()         // overflow: ellipsis, single line
    .whitespace_nowrap()
```
