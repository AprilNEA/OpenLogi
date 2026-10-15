---
name: gpui-kit-design-guides
description: The normative Design Guides for GPUI Kit desktop applications. Read in full before designing or changing any screen, layout, spacing, visual hierarchy, color, density, component choice, interaction state, overlay, motion, data-heavy view, or interface copy in a GPUI Kit (gpui-kit / gpui-component) application, and before reviewing UI work for design quality. Also use when asked what the design rules are, or whether a UI decision follows them.
---

# GPUI Kit Design Guides

The guide is [references/design-guides.md](references/design-guides.md). It is
a requirement, not inspiration. Read the guide file itself before doing UI
work. Do not answer from this page, from an existing screen in the codebase, or
from training data.

If the file is missing, fetch `https://gpui-kit.com/docs/design-guides.md`.
The guide is a verbatim copy of that page, so its link to `./coding-guides.md`
means the Coding Guides in the `gpui-kit` skill (or `https://gpui-kit.com/docs/coding-guides.md`).

## How to read it

Read the whole guide for a new screen or a redesign. For a narrow change, read
"Design thesis" and "Start from the task" first, then the section for the
change. Sections, in order (`grep -n '^## ' references/design-guides.md`):

| Section                              | Read when                                                              |
| ------------------------------------ | ---------------------------------------------------------------------- |
| Design thesis                        | Always                                                                 |
| Learning from Shadcn                 | Choosing what to borrow from web component libraries                   |
| Start from the task                  | Always; task hierarchy, interaction promise, what to leave out         |
| Visual language                      | Color, typography, spacing, radius, borders, elevation, density, icons |
| Layout patterns                      | Window structure, sidebars, toolbars, panels, forms, resizable regions |
| Components and composition           | Picking a component, composing parts, when to build a new one          |
| Interaction states                   | Hover, focus, pressed, selected, disabled, loading, validation, danger |
| Feedback and overlays                | Dialog, sheet, popover, menu, notification, tooltip, dismissal, focus  |
| Motion                               | Any animation or transition                                            |
| Designing data-heavy interfaces      | Tables, lists, trees, dashboards, dense inspectors                     |
| Interface language                   | Any user-facing text: labels, buttons, titles, errors, empty states    |
| Internationalization and platform fit | Multi-locale copy, Chinese terminology, macOS/Windows conventions     |
| Guidance for AI-generated interfaces | Always when an agent produces UI                                       |
| Accessibility checklist              | Before finishing                                                       |
| Design review checklist              | Before finishing; run every item against the work                     |

## Non-negotiables

A floor, not a substitute for the guide.

- **Desktop before web convention.** Keyboard access, window chrome, menus,
  dense data views, resizable regions, persistent navigation.
- **`Button` vs `Link`.** `Button` for every in-app command, `ghost` or
  `outline` when it should read quietly. `Link` only for external URLs and
  email addresses.
- **Tokens before values.** No raw hex or `rgb(...)`; use `cx.theme()`
  semantic tokens and rem-based helpers. Any spacing number quoted in the guide
  is the current default scale, not a literal to repeat.
- **State must be visible.** Hover, focus, selection, disabled, loading,
  validation, and destructive states each need distinct, consistent treatment.
- **Overlays.** Escape dismisses the topmost surface and returns focus to its
  trigger.
- **Copy.** Name the object and the verb: `Delete "Roadmap"?` with a `Delete`
  button, not `Are you sure?` with `OK`.

Finish by running the Design review checklist against the work.
