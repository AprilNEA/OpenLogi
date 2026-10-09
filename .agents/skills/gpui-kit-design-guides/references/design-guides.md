---
title: Design Guides
description: Product and interaction design guidance for GPUI Kit applications
order: -2.1
---

# Design Guides

Use this guide before choosing components or writing layout code. It records
the product judgment accumulated through years of GPUI Kit desktop work:
an interface should feel native, restrained, precise, and understandable
without guesswork.

This is a normative guide. **Must** identifies a correctness or ecosystem
constraint, **should** is the default that needs a concrete reason to override,
and **may** is an optional technique. Component API documentation remains the
authority for individual methods.

The rules build on behavior in `gpui-base`, the GPUI Component theme and
component system, and familiar desktop interaction. Shadcn contributes useful
methods—open code, composition, and dependable defaults—but does not determine
how a GPUI application should look. When influences conflict, preserve GPUI's
lifecycle constraints and the interaction people already understand.

## Design thesis

Build interfaces that feel native, quiet, and precise. Let content, hierarchy,
and interaction carry the experience; decoration should support them rather
than compete with them.

1. **Clarity before personality.** Make the primary task and next action clear
   before adding brand expression.
2. **Composition before invention.** Start with established components and
   compose them into product-specific workflows. Create a new primitive only
   when its behavior is genuinely new.
3. **Tokens before values.** Colors, radii, typography, and spacing should form
   a system. Avoid isolated literals that cannot respond to themes.
4. **Desktop before web convention.** Preserve keyboard access, window chrome,
   menus, dense data views, resizable regions, and persistent navigation where
   the task benefits from them.
5. **State must be visible.** Hover, focus, selection, disabled, loading,
   validation, and destructive states need distinct and consistent treatment.

## Learning from Shadcn

Shadcn's most useful contribution is not a particular border color. It is a
way of building a system:

- own the top layer of the interface instead of fighting a sealed abstraction;
- compose small, predictable parts into product-specific components;
- provide defaults that already form one visual language;
- keep the code and composition legible to both people and AI;
- separate behavior primitives from the styled layer.

GPUI Kit applies those ideas through a Rust library and the split between
`gpui-base` and `gpui-component`. Applications normally compose or wrap the
published components; contributors move genuinely reusable behavior into Base
and keep visual policy above it.

Do not copy these web assumptions blindly:

| Web habit | Native GPUI default |
| --- | --- |
| Pointing-hand cursor on every button | Default arrow cursor; pointing hand for links |
| Page navigation as the main structure | Persistent windows, panes, sidebars, tabs, and menus |
| Browser focus and scrolling as a fallback | Explicit focus ownership and region-owned scrolling |
| Mobile-first single column | Resizable desktop shell with a defined minimum window size |
| Hover-revealed critical actions | Keyboard- and pointer-reachable actions that do not depend on hover |
| A row of hover-only icon buttons | A visible primary action plus `DropdownMenu` or `ContextMenu` for secondary commands |
| Link-styled text for application commands | `Button`, `outline`, or `ghost`; Link only for URLs, web resources, or email addresses |
| Large touch density everywhere | Medium density by default; compact only where information work benefits |
| CSS overrides across descendants | Typed builders, semantic parts, and application composition |

## Start from the task

Before drawing a screen, write down:

- the user's primary task;
- the object being viewed or changed;
- the actions that must remain immediately available;
- the information required to make a decision;
- empty, loading, error, offline, read-only, and permission-denied states;
- the keyboard path through the workflow.

Organize the window around those answers. Do not begin with a dashboard grid or
a component catalogue. A good desktop interface exposes the user's mental
model: documents, accounts, projects, messages, settings, or another stable
object—not the internal service architecture.

Design the primary task before distributing controls. Its visual weight,
location, and information depth should match its importance to the product. A
core result must not be reduced to a small count, an icon in a corner, or a
weak footer action while secondary content consumes the page. When a result set
is the product's main value, consider a summary region or card that exposes the
count, representative results, meaningful state, and a clear next action.

For every proposed action, name its visible object, current state, scope, and
result. If the interface does not show or clearly imply those things, the
action is premature. Do not expose a capability merely because the backend has
it; first design how the object enters the user's mental model.

## Visual language

### Hierarchy

Prefer a small number of clear levels:

- **window or page title** identifies the current object or workspace;
- **section title** separates meaningful regions;
- **body text** carries the work;
- **muted text** provides secondary metadata and help;
- **labels** identify controls and values.

Use size, weight, spacing, and separators before adding color or containers.
Avoid nesting cards inside cards: most desktop regions need only a background,
a hairline boundary, and intentional spacing.

Evaluate hierarchy across the whole feature, not component by component. Hide
accent color and decoration during review: the primary task, current selection,
result summary, and next action should still be obvious from structure. A
screen that contains individually plausible controls can still fail when they
do not form one reading order and one decision path.

Treat emphasis as a limited budget. A local surface needs one clear focal point,
not a field of competing highlights. If everything is colored, badged, bold,
boxed, or promoted to an alert, nothing reads as important. Establish priority
with structure and proximity first; spend stronger color and components only
where a distinction changes what the user notices or does.

### Color and themes

Read colors from `cx.theme()` and use them by semantic role:

- `background` and `foreground` for the main surface and text;
- `group_box`, `popover`, `sidebar`, and their foreground tokens for their
  named surfaces;
- `muted` and `muted_foreground` for supporting information;
- `primary` for the principal action or selection emphasis;
- `danger`, `warning`, `success`, and `info` only for their meanings;
- `border`, `input`, and focus-ring tokens for structure and interaction.

Do not use a semantic status color as decoration. Do not encode meaning by
color alone. Verify every custom surface in light and dark themes and with
custom theme values; never assume that foreground is black or background is
white.

Use Badge for a short state, count, or classification that benefits from rapid
scanning—not for every label, metadata value, filter, or section title. Keep
most badges neutral; reserve semantic variants for states that truly carry
success, warning, danger, or informational meaning. A row of multicolored
badges is usually a missing hierarchy or grouping decision.

Application UI should not contain raw hex, `rgb`/`rgba`, or `hsla` colors.
Resolve colors from `cx.theme()` by semantic role. If the required role does
not exist, define it in the product's theme/token layer rather than embedding a
palette value at the call site. Raw colors belong only inside theme definitions
or in audited data/raster content whose color is itself the data.

### Radius, spacing, and density

Derive corner radii from the active theme. This preserves a product's ability
to become square or more rounded as one coherent system. Use `radius_full()`
for circles and pills rather than a literal maximum radius.

Use a compact spacing scale and repeat it. Related label/control pairs should
be closer than separate groups; separate groups should be closer than separate
sections. Prefer component sizes (`xsmall`, `small`, default medium, `large`)
over one-off heights. Use compact variants for toolbars and data-dense screens,
not to squeeze an unclear layout into less space.

The shared semantic scale is intentionally small: spacing progresses through
roughly 2, 4, 8, 12, 16, 24, and 32 pixels, while typography stays near 12,
14, 16, 18, and 20 pixels. Treat these as relationships rather than permission
to scatter their current values through feature code. GPUI Component currently
projects a fixed default `SpacingTokens` scale from its global `Theme`; unlike
colors and radii, `Theme::apply_semantic_tokens` does not persist a custom
spacing scale. An application that needs different spacing must own that full
token snapshot and use it consistently in its application components.

### Spatial grammar

Spacing expresses relationship. Choose a gap from the semantic scale by asking
what the two things mean to each other:

| Relationship | Typical token | Current scale | Examples |
| --- | --- | --- | --- |
| Optical correction | `xxs` | 2 px | icon baseline, compact separator |
| Parts of one control | `xs` | 4 px | menu icon/label, title/description |
| Closely related controls | `sm` | 8 px | button icon/label, dialog actions |
| One content group | `md` | 12 px | notification columns, compact form rows |
| Separate groups in one section | `lg` | 16 px | panel padding, form groups |
| Separate sections | `xl` | 24 px | major blocks in a page or inspector |
| Major region boundary | `xxl` | 32 px | empty-state breathing room, page bands |

These values describe the current default scale, not literals to repeat. Use
`cx.theme().spacing_tokens()` or the corresponding GPUI scale helpers for the
ecosystem default. A product-owned scale should preserve the ordering and
relationships and must be passed through the application's own design-system
context rather than assumed to persist in the global GPUI Component theme.

Use these rules when resolving horizontal and vertical space:

1. **Inside before outside.** A component's padding belongs to the component;
   the gap between components belongs to their parent.
2. **Vertical rhythm shows grouping.** The gap between a title and its
   description is smaller than the gap from that description to the next
   section. Equal gaps imply equal relationships.
3. **Horizontal space supports scanning.** Repeated rows keep icons, labels,
   values, badges, and trailing actions on stable columns.
4. **Leading and trailing are semantic.** Think in reading-order edges even
   when the current API uses left/right; this keeps future RTL adaptation
   possible.
5. **Do not double padding.** A card placed in an already padded panel should
   not automatically add another full panel inset.
6. **Use optical alignment sparingly.** A one- or two-pixel correction is valid
   for icon or glyph geometry, but document why it differs from the scale.

Common compositions in the current system illustrate the relationships:

- button contents use 4 px at small sizes and 8 px at normal sizes;
- dialog headers and footers use an 8 px internal gap;
- compact list and menu rows use 4 px vertical and 8–12 px horizontal padding;
- sheet headers use about 16 px leading and 12 px trailing space, leaving room
  for a close affordance, while footers use 16 px horizontal and 12 px vertical
  space;
- notifications use 16 px horizontal padding and a 12 px column gap because
  icon, message, and action are distinct groups.

Do not treat these as copy-and-paste recipes for every surface. They reveal the
system: controls are tighter internally, rows optimize scanning, and
containers spend more space at their boundary than between their contents.

### Proportion and layer hierarchy

Start with content requirements, then set proportions. Avoid arbitrary halves
when one pane has a clearly different role.

- A navigation sidebar should be wide enough for stable labels but visibly
  subordinate to the work area. Give it a minimum, preferred, and maximum
  width rather than a percentage alone.
- In master–detail layouts, let the collection remain scannable and give the
  detail pane the surplus. A roughly one-third/two-thirds starting point is
  often useful, but content constraints are authoritative.
- Inspectors and auxiliary sheets should not cover the primary object by
  default. They should be resizable or dismissible when their content grows.
- Dialog width comes from the decision: short confirmation, medium form, or a
  dedicated window/page for complex work. Do not enlarge a dialog simply to
  create whitespace.
- Reserve the strongest elevation for the topmost decision layer. Within a
  layer, use background and hairlines—not successively larger shadows—to show
  hierarchy.

Define three size constraints for every major region: the minimum at which its
task still works, a comfortable default, and how it consumes surplus. Persist
user-controlled splits when they represent workflow preference, and clamp
restored values against the current window.

### Alignment details

Alignment is a structural system, not a final polish pass. Establish a small
set of alignment spines for each surface: shared leading and trailing edges,
text baselines, center lines, and fixed functional lanes. Elements at the same
level should attach to the same spine from top to bottom or leading to trailing,
even when they are different component types.

<img class="alignment-light" src="/alignment-spines.svg?v=20260822-4" alt="Alignment spines across a desktop surface">
<img class="alignment-dark" src="/alignment-spines-dark.svg?v=20260822-4" alt="Alignment spines across a desktop surface">

Vertical red lines sit beside shared edges or control centers for content,
status, time, and trailing actions. Horizontal lines sit beneath text baselines
or pass through a row center to show bottom and vertical-center alignment. The
compact comparison isolates a one-rendered-pixel drift that must be corrected
at its structural owner.

- Give sibling regions a shared content inset. A heading, toolbar, list row,
  empty state, and footer that describe the same level should not each invent a
  slightly different leading edge.
- Repeat column geometry through the whole region. Headers, rows, summaries,
  loading states, and inline editors should reserve the same lanes for identity,
  metadata, status, numbers, and actions.
- Align related controls across rows and sections. Form labels, fields,
  descriptions, and validation messages should reveal a stable vertical grid
  when the page is scanned from top to bottom.
- Keep horizontal bands coherent. Items sharing a toolbar, title bar, status
  bar, or row should use one baseline or center line instead of individually
  tuned offsets.
- Introduce indentation only for real hierarchy, containment, or disclosure.
  Decorative indentation makes siblings look subordinate and breaks the
  surface's reading line.
- When a nested level ends, return exactly to the parent spine. Do not let
  accumulated padding drift across nested containers.
- Preserve the spine through optional content. Missing icons, badges,
  descriptions, or trailing actions must not move the remaining labels; use
  intentional slots or lanes when cross-row comparison matters.
- Align major regions with one another where their hierarchy matches. Sidebar
  headers, content titles, split panes, toolbars, and bottom bars need not share
  every coordinate, but coincident levels should form visible continuous lines.

Not every edge should align. A child can indent, a primary value can lead its
supporting metadata, and a destructive decision can gain separation. Such
exceptions must communicate hierarchy or meaning; they must not result from
uncoordinated component padding. Start with the shared spine, then make the
exception explicit.

Treat exact alignment and repeated gaps as quality invariants. When two edges
or spaces are intended to be equal, a one-rendered-pixel difference is a defect,
not an acceptable optical approximation. Inspect resolved bounds with a
measurement tool at representative window sizes, zoom levels, and display scale
factors. Compare coordinates and distances; do not approve alignment only from
a casual screenshot.

The rendered-pixel tolerance is a verification rule, not permission to patch
the code with raw pixel offsets. Equal relationships should resolve from the
same `rem` helper, spacing token, grid definition, or shared component inset.
Fix the common owner when they differ. Account for fractional layout and device
rounding so intended spines land on the same physical pixel instead of drifting
at particular zoom levels.

- Align text by baselines, not by bounding-box centers, when mixed sizes share
  a row.
- Center icons in a fixed slot so labels do not move when icons differ in
  intrinsic width.
- Right-align comparable numbers; left-align prose and identifiers unless the
  locale requires otherwise.
- Keep trailing row actions and disclosure indicators in fixed-width lanes.
- Align form controls by their interactive frame, not by help text below them.
- Use `justify_between` only when the two sides truly own opposite edges; it
  should not disguise missing structure in the middle.
- Hairlines belong on the boundary owner. Two adjacent regions must not each
  draw the same separator.
- A scrollbar belongs to the region that scrolls and sits against that panel,
  editor, or window's trailing edge. Content padding may inset text and rows;
  it must not pull the scrollbar into the middle of the surface. Reserve a
  deliberate scrollbar gutter when content needs clearance.

### Density tiers

Medium is the ecosystem default. Change density for the whole local context,
not one isolated control:

- **comfortable / large:** onboarding, sparse forms, prominent decisions;
- **standard / medium:** most application chrome and workflows;
- **compact / small:** toolbars, menus, tables, and repeated professional data;
- **extra compact / xsmall:** exceptional high-density utilities, never the
  automatic choice for an entire application.

The current controls demonstrate a bounded scale rather than arbitrary sizing:
buttons commonly move through approximately 20, 24, and 32 px frames; input
and data controls may extend to about 44 px at large size; table rows use about
26, 30, 32, and 40 px. Use the component's `Size` API so typography, icon,
padding, and hit target change together. A custom height that changes only the
outer box is usually incomplete.

### Zoom, base font, and `rem`

A well-designed `rem` system preserves hierarchy while the interface zooms.
Zoom is successful when the relationship between title and body, control and
icon, inner and outer spacing, primary and secondary regions still feels the
same at every scale—not merely when every object becomes larger.

GPUI Component adopts the relative-scale idea familiar from Tailwind. The
theme's base `font_size` becomes the window's `rem` through `Root`, and GPUI
scale helpers such as `text_sm()`, `gap_2()`, `p_4()`, `h_8()`, and `size_4()`
resolve against it. This gives typography, spacing, controls, and icons one
shared zoom axis.

Design in ratios:

- type steps keep the same hierarchy around the base body size;
- spacing steps keep the same grouping relationships around the type;
- control frames, icons, and hit targets scale with their labels;
- pane minima and comfortable widths account for the scaled content;
- corner radii and focus treatment remain optically consistent with the
  control frame.

Do not implement zoom by changing text size alone. A larger label inside a
fixed-height button, a larger document inside fixed pane minima, or larger rows
inside a stale virtual-list measurement destroys the original rhythm and can
clip content. Conversely, multiplying every physical pixel—including
hairlines—can make the interface visually heavy.

As a rule, application layout should not call `px(...)` directly. Use GPUI's
rem-based scale helpers (`p_2`, `gap_3`, `w_64`, `text_sm`, and related
builders) or semantic component sizes. Use fixed pixels only when the value
represents a physical or raster boundary:
a one-device-pixel hairline, platform window inset, bitmap dimension, minimum
hit-test tolerance, or geometry that must match an external surface. These are
audited, documented exceptions. Product spacing, typography, icon size, and
ordinary control geometry stay on the relative scale.

Test interface zoom at several base-font values, not just the default. Verify
hierarchy, wrapping, truncation, minimum window size, pane resizing, focus-ring
clearance, popup placement, and virtualized row measurement. Also distinguish
interface zoom from Dock's panel zoom: Dock zoom makes one container fill its
area while retaining its chrome; it does not change `rem` or application scale.

### Surfaces and elevation

Use elevation to explain stacking, not importance. The base window surface is
flat; separators and background contrast define its regions. Popovers, menus,
dialogs, and notifications may use progressively stronger shadows because they
sit above other content. Do not put a shadow on every card.

All surfaces of the same kind should share one treatment. GPUI Component, for
example, deliberately gives popup families one themed popover surface so
Popover, Select, Combobox, DatePicker, and menus do not drift apart. When an
application invents another anchored surface, reuse that semantic treatment
instead of approximating it with unrelated border and shadow literals.

### Typography and icons

Use the platform UI font for interface text and monospace only for code,
identifiers, shortcuts, and aligned numeric data. Keep body text readable and
avoid excessive uppercase or letter spacing, especially for CJK text.

Use one icon family in a product. Icons supplement labels; they should not
replace unfamiliar actions with guesswork. Icon-only buttons require a tooltip
and an accessible name. Use filled or colored icons to communicate a state,
not merely to make a toolbar lively.

## Layout patterns

### Choose a stable shell

Most applications should use one of these shells:

- **single workspace:** toolbar or title bar above one primary view;
- **sidebar workspace:** persistent navigation beside a changing detail view;
- **master–detail:** resizable collection and detail panes;
- **document workspace:** tabs or a dock area for multiple long-lived objects;
- **utility window:** one focused task with a short, fixed action path.

Keep global navigation stable while content changes. Give the primary work area
the remaining space with `flex_1()` and `min_w_0()` / `min_h_0()` where
overflowing children must shrink. Use `Scrollable`, `VirtualList`, `Table`, or
`DockArea` for their intended behavior instead of rebuilding scrolling or pane
management from nested `div`s.

### Responsive desktop windows

Desktop does not mean fixed-size. Decide what happens as a window narrows:

1. preserve the primary task;
2. allow resizable regions to reach a documented minimum;
3. collapse secondary labels or inspectors;
4. move low-frequency actions into a menu;
5. scroll only the region whose content actually overflows.

Do not hide an action without providing another path to it. Avoid making the
entire window scroll when only a list or document body should scroll.

GPUI flex layouts have the same intrinsic-size pressure found in other layout
systems: a `flex_1()` child may still refuse to shrink around long content.
Design and implementation must agree on which panes may shrink, truncate, wrap,
or scroll. A clipped region also clips an outward focus ring; never trade away
keyboard visibility merely to simplify overflow.

### Forms and settings

Use a visible label for each field and place help or validation next to the
field it describes. Align related fields, but do not force long labels into a
narrow fixed column. Use the appropriate control: `Checkbox` for independent
choices, `RadioGroup` for a small visible set, `Select` for a longer set, and
`Switch` for a setting that takes effect immediately.

Disable submission while an operation is in flight, keep the user's input, and
show the result near the action. Reserve dialogs for short, focused decisions;
use a full page or sheet for workflows that need exploration or many fields.

## Components and composition

Follow the Shadcn principle that components are building material rather than a
sealed design system. GPUI Component supplies coherent defaults, while the
application owns composition and product semantics.

- Use component variants by meaning. Primary is reserved for the explicit
  default commit in a decision area—normally the action invoked by Enter. A
  lone, frequent, or desirable action is not automatically primary. An `Add`
  command in a management toolbar normally uses a default Button; a form's
  default `Create` commit may use primary. Use `danger` for destructive
  commitment and `ghost` for quiet toolbar actions.
- Prefer explicit compound parts and render callbacks over styling arbitrary
  descendants.
- Keep a repeated pattern consistent across the product. Wrap it in an
  application component when it carries domain language or policy.
- Use the standard component for its semantic role. A menu, dropdown menu,
  popover, select, and command palette are not interchangeable boxes; each owns
  different selection, focus, keyboard, dismissal, and layout contracts.
- Preserve the component family's geometry. Menu rows share vertical and
  horizontal padding, height, icon and checkmark slots, separators, radius, and
  state treatment. Do not imitate one menu with a custom popup whose spacing
  only approximates the system.
- Do not wrap a library component merely to rename every method or freeze all
  of its capabilities.
- Move reusable behavior without product styling to `gpui-base`; keep themed,
  opinionated presentation in GPUI Component or the application.

## Interaction states

### Make the result understandable before the click

A control should predict its result. Use familiar desktop controls and
placement so people can act without learning the interface first. Its label
names the action and object, its state shows availability, and its feedback
confirms the same outcome.

Do not label a Button `Save` if it opens a configuration flow, or `Delete` if
it only removes an item from a group. Name the scope when context does not make
it clear. Respond immediately to activation, prevent duplicate submission
during longer work, and show the result near the object that changed. Add a
success message only when the result itself is not visible.

Every interactive control should be designed for:

| State | Design requirement |
| --- | --- |
| Rest | Clear affordance without visual noise |
| Hover | Subtle pointer feedback, never the only cue |
| Pressed | Immediate press feedback |
| Open / pressed | Persistent feedback while an attached popup is open |
| Focus visible | High-contrast keyboard focus ring |
| Selected / checked | Persistent state distinct from hover |
| Disabled | Lower emphasis and no misleading hover/pressed response |
| Loading | Preserve context, prevent duplicate action, explain long waits |
| Error | State what happened and how to recover |

Use GPUI's focus system and Actions for commands that should work from the
keyboard. Match familiar desktop shortcuts, expose shortcuts in menus or
tooltips, and keep focus in a logical place after opening or dismissing an
overlay.

Selection is part of the information model, not optional polish. Tabs,
segmented choices, selectable rows, filters, and navigation destinations must
show a persistent selected state. A Button that owns a dropdown must remain
visibly pressed or open until the popup closes; hover alone cannot explain the
relationship between trigger and surface.

For destructive actions, distinguish between reversible and irreversible work.
Prefer undo or a temporary notification for reversible changes. Use an
`AlertDialog` when the consequence is serious and cannot be undone; name the
specific object and consequence in the confirmation copy.

### Pointer conventions

Use the default arrow cursor for buttons, checkboxes, menu items, tabs, and
other native controls. Use a pointing hand for links and content that behaves
as a link. Use text, resize, grab, and prohibited cursors only when they
describe the active manipulation. A cursor reinforces an affordance; it does
not replace the control's visible state or accessible role.

Keep hover effects modest because keyboard and accessibility interaction has no
hover. Do not reveal the only copy of a destructive or essential action on
hover. Contextual row actions may become quieter at rest if the same commands
remain available through selection, keyboard, or a context menu.

### Prefer desktop command surfaces over hover toolbars

Use command frequency and scope to choose where an action lives:

- keep the primary or frequent action visible as a labeled Button or familiar
  toolbar control;
- put secondary actions for the current region behind a visible
  `DropdownMenu` trigger;
- put commands that act on the object under the pointer in a `ContextMenu`;
- expose the same important command through an Action/key binding when it has a
  natural keyboard form;
- use a hover-revealed icon only as a shortcut to a command that remains
  reachable elsewhere.

This is more than a visual preference. GPUI Component's menu system already
owns directional keyboard navigation, confirmation and cancellation, disabled
items, separators, submenus, shortcut presentation, focus transfer and
restoration, and nested-menu dismissal. A custom strip of hover buttons must
rebuild those behaviors and is invisible to keyboard-only and many assistive
technology workflows.

Choose `DropdownMenu` when users need a visible indication that more commands
exist—for example a toolbar overflow, document actions, or account menu. Choose
`ContextMenu` for selection- or object-scoped commands such as rename,
duplicate, reveal, or remove. The context menu must not be the only way to
perform an essential command; provide a menu-bar, toolbar, keyboard, or detail
view path as appropriate.

Do not put every action into a menu to make a screen look minimal. Discovery
and speed matter: the main action stays visible, dangerous items remain clearly
labeled and separated, and a menu item should use the same verb, icon, shortcut,
enabled state, and result everywhere it appears.

### Button means application action; Link means external resource

Use a Button when activation changes application state, confirms a decision,
opens a tool, submits data, or runs a command. Choose its treatment by local
hierarchy:

- primary Button for the one emphasized commitment in a decision area;
- default Button for ordinary visible actions;
- outline Button when an action needs a clear boundary with less emphasis;
- ghost Button for familiar, low-emphasis toolbar and inline actions;
- icon Button only for a well-known symbol, with an accessible name and tooltip.

Do not assign primary because a Button is the only action on screen, because it
is placed at the top right, or because the team wants more clicks. Primary
communicates default commitment and keyboard behavior. If activation is merely
an ordinary command such as adding an item, opening a tool, or refreshing a
view, use a default, outline, or ghost Button according to its local hierarchy.

Use an underlined Link only for an external resource target: a URL, web page,
online documentation, or email address. It uses the pointing-hand cursor
because its contract is leaving the current application context for that
resource. Do not use Link styling to make a functional command look quiet. A
link-shaped Delete, Save, Refresh, Add, Open-menu, or in-app navigation action
hides the control's affordance and exposes the wrong accessibility role.

“View” does not make an in-app destination a Link. A full report, analysis,
details panel, or local record still opens through a Button, row, card, tab, or
disclosure control. Use concise context-aware labels such as `Full analysis`
when the containing card already establishes what opens; reserve underlining
for a resource that actually opens in a browser or mail client.

All internal navigation—sidebar rows, tabs, breadcrumbs, list items, opening a
local view, or switching workspaces—must use the corresponding native component
or a Button/Action. Visual emphasis is chosen through Button variant or the
navigation component's selected state, never by lying about semantics.

## Feedback and overlays

Choose the smallest surface that fits the decision:

- tooltip: a short explanation or shortcut;
- popover: contextual controls that do not interrupt the task;
- menu: a compact list of actions;
- notification: asynchronous status that does not require a decision;
- dialog: a focused decision or short form;
- alert dialog: explicit confirmation of a consequential action;
- sheet: supplementary work that benefits from more persistent space.

An Alert interrupts the visual hierarchy even when it does not open a modal.
Use it for important, exceptional information that needs attention in the
current task, not as a decorated container for ordinary descriptions, tips, or
empty space. Prefer inline help, muted text, or a normal section when the
content does not require immediate notice or action.

Avoid stacking overlays. Escape should dismiss the topmost dismissible layer,
and focus should return to the trigger or the next logical target.

An overlay action must refer to an object or state the overlay actually shows.
For example, expose `Clear history` only when a distinct recent-history section
is visible and contains entries. Search results, recent items, and favorites
are different collections; label and separate them instead of merging them
into one unexplained list. Hide an inapplicable action or disable it with a
useful reason—do not park an ambiguous trash icon in a footer.

Footer space is not a catch-all for capabilities that lacked a place in the
design. A footer may present shortcuts, status, or actions that apply to the
whole surface, but each item must answer: what is its object, why is it
available now, what scope does it affect, and what visible state changes after
activation?

## Motion

Motion explains change; it is not ambient decoration. Use short transitions for
appearance, dismissal, expansion, and spatial continuity. Avoid animating large
layout changes when opacity or transform communicates the same relationship.
Honor reduced-motion preferences, never require animation to understand state,
and do not add a default animation to every component.

Motion policy belongs to the styled or application layer. Base may own the
lifecycle mechanism or geometry needed for a transition, but it should not
decide that every product fades or slides. Give independently animated values
stable identity, and make interruption reverse smoothly from the currently
sampled value rather than restarting from an old endpoint.

## Designing data-heavy interfaces

Dense does not mean cramped. In tables, trees, command palettes, editors, and
docks:

- keep headers and primary row identity visually stable;
- align comparable values and use tabular numerals where appropriate;
- distinguish focus, hover, active row, and multi-selection;
- keep sorting and filtering visible and reversible;
- preserve selection by domain identity across filtering and reordering;
- virtualize large collections without changing keyboard semantics;
- use progressive disclosure for secondary columns and inspectors;
- provide a useful empty state that explains the next action.

Choose a table for comparison across consistent fields, a list for scanning
heterogeneous items, a tree for real hierarchy, and a dock only when users need
to arrange long-lived tools or documents. Do not use a complex data component
as a visual style.

## Interface language

Words are part of the interface architecture. Write the vocabulary for a
feature as a system—destinations, objects, commands, states, and outcomes—not as
isolated translations of implementation features. Prefer the shortest wording
that remains accurate in its actual context.

### Let context carry context

Do not repeat information that the surrounding surface already establishes. A
sidebar destination is usually the object or domain itself: use `Users`, not
`User Management`; `Shortcuts`, not `Shortcut Configuration Management`. A
column whose rows already contain actions can omit a generic `Operation`
heading. A dialog titled `Delete “Roadmap”?` does not need body text that asks
the same question again.

This is context economy, not deletion for its own sake. Add text when it changes
the decision: identify the affected scope, an irreversible consequence, an
unusual prerequisite, or a way to recover. Every extra word should answer a
question the current layout does not already answer.

Use nouns for destinations and objects (`Users`, `Appearance`, `Orders`), verbs
for commands (`Save`, `Duplicate`, `Export`), and adjectives or short phrases
for states (`Offline`, `Up to date`, `Pending review`). Avoid wrappers such as
`Management`, `Module`, `Page`, `Function`, `Operation`, and `System` unless the
word distinguishes a real domain concept.

### Write each language, do not translate its shape

Start from shared intent, hierarchy, and terminology, then compose each locale
as natural interface language. Do not preserve the source language's word
order, number of words, politeness filler, or grammatical category. English
`Users` can express a Chinese feature concept that would literally expand to
“user management”; fidelity means preserving purpose, not preserving tokens.

Remove words supplied by the enclosing information architecture. Inside a
`Settings` surface, a destination is often simply `Account`, not `Account
Settings` and never the unnatural singular `Account Setting`. The correct
English label is chosen from its role and neighbors, not from the standalone
source phrase.

Maintain a small product lexicon for recurring objects, commands, and states.
Use the same term in the toolbar, menu, context menu, dialog, shortcut search,
and documentation unless the context genuinely changes its meaning. Review
copy in the rendered surface: neighboring labels often reveal repetition or
inconsistent scope that a locale file cannot.

In localized technical writing, preserve an established framework term when a
translation would be less precise. Keep API identifiers in their original form
and format them as code. Do not retain ordinary foreign words merely to sound
technical. Explain a retained term on first use when needed, then use the same
form throughout the interface, documentation, and API examples.

### Buttons and confirmation dialogs

Button labels are short by default—usually one or two words—and describe the
result, not the gesture or the component. Prefer `Save`, `Move`, or `Delete` to
`Click to save`, `Perform move`, or `Confirm deletion`. Use `Cancel` consistently
for the action that leaves without committing. Reserve `OK` for acknowledging
purely informational content.

Short is a default, not a character limit. A deliberately longer label is
better when its words expose a consequence or distinguish choices that users
could otherwise confuse, for example `Delete from this group` versus `Delete
everywhere`, or `Restart without saving`. Length must buy decision-critical
information; it must not restate the dialog title or body.

Use the most specific concise result as the confirmation label when possible:

| Context | Weak | Prefer |
| --- | --- | --- |
| Delete dialog | `Yes`, `Sure`, `Confirm deletion` | `Delete` |
| Unsaved changes | `Confirm`, `Yes` | `Discard changes` |
| Pure acknowledgement | `Confirm operation` | `OK` or `Done` |
| Complex consent whose result has no clear verb | `Yes` | `Confirm` |

`Confirm` is a useful fallback when the surrounding dialog fully names a
complex commitment and no shorter result verb is accurate. It should not
replace a clear command. `Sure` is conversational rather than a stable English
command and is too ambiguous for the standard vocabulary.

A confirmation dialog should form one compact decision:

- title: the decision or condition, such as `Delete “Roadmap”?`;
- body: only new scope, consequence, or recovery information;
- actions: `Cancel` and the result, such as `Delete`;
- destructive styling: applied to the destructive result, not substituted for
  precise wording.

Avoid generic titles such as `Notice`, `Warning`, `Error`, and `Confirmation`
when the actual condition can be named. Avoid ritual phrases such as “Are you
sure you want to…”, “Would you like to…”, “Please note that…”, and “successfully”
when the structure or state already communicates them. Courtesy should come
from a calm, respectful tone, not repeated `please`.

### Capitalization, punctuation, and symbols

Use sentence case for English UI by default: `Reset layout`, not `Reset Layout`
or `RESET LAYOUT`. Preserve proper nouns and established acronyms. Follow a
platform convention such as title case for native menu commands only when the
platform integration benefits from it, and apply that convention consistently
within the component class.

ALL CAPS can provide restrained typographic emphasis for very short section
labels, eyebrows, statuses, established acronyms, and code-like identifiers.
Its compact shape and measured tracking can form a level similar to bold type,
but it does not belong on Buttons, long headings, sentences, or dense lists. Do
not combine uppercase, strong color, and bold weight in the same region, and do
not transform every string automatically: product names, acronyms, and localized
content must preserve their intended casing.

Labels, buttons, menu items, tabs, headings, placeholders, and short states do
not take a final period. Complete explanatory, warning, and error sentences do.
Avoid exclamation marks in routine success and failure messages. In Chinese,
use full-width punctuation in sentences and omit terminal punctuation from
short control labels by the same semantic rule.

Use the single ellipsis character (`…`), not three periods. Append it to every
Button or MenuItem that opens a dialog, sheet, or separate window, and to a
command that requires more input or choices before it can complete, such as
`Settings…` or `Export…`. An immediately executed command does not take an
ellipsis. Use an indeterminate progress indicator, not decorative dots, to
communicate ongoing work.

Errors should say what happened and, when useful, the next recovery action.
Success feedback should name the resulting state only when that state is not
already visible. Prefer `Couldn’t save. Check your connection and try again.`
to a technical code or a long apology; omit a `Saved successfully` toast when
the document visibly becomes saved.

## Internationalization and platform fit

Copy must survive expansion, CJK typography, and different shortcut notation.
Do not size a control from one English label. Keep text out of raster assets,
avoid concatenating translated fragments, and let labels wrap or truncate only
where the product defines a recovery path such as a tooltip.

Respect platform differences that carry meaning: Command versus Control,
native window decorations, system appearance, scrollbar behavior, menus, and
notification capabilities. Keep the product's information architecture stable
across platforms, but do not erase familiar platform behavior for superficial
pixel equality.

## Guidance for AI-generated interfaces

An AI changing a GPUI interface should first inspect the nearest feature,
theme tokens, and component documentation. It should state the primary task,
state owner, component composition, and keyboard path before generating code.
It must not infer an API from React/Shadcn examples or invent a GPUI method
because the name seems plausible.

AI output is incomplete until a human can explain why the hierarchy, density,
component choice, and exceptional literal values belong in this product. A
visually plausible screenshot is not proof: keyboard behavior, focus, dynamic
content, themes, resizing, and failure states are part of the design.

## Accessibility checklist

Before considering a screen complete, verify that:

- every action is reachable and operable by keyboard;
- focus order follows visual and task order;
- focus remains visible and is restored after overlays;
- controls have names, and icon-only controls have tooltips;
- text and meaningful boundaries have sufficient contrast;
- status is not communicated by color alone;
- disabled and read-only states are distinguishable;
- labels, errors, and descriptions remain near their controls;
- content remains usable with longer translations and larger text;
- pointer targets are comfortably sized even in a dense layout.

## Design review checklist

A review does not inventory components; it judges whether the interface made
the right decisions. Ask, in order:

1. **Is the task clear?** Can a new user recognize the purpose, primary action,
   and next step without learning, guessing, or experimenting?
2. **Does every action keep its promise?** Do the label, control, state, scope,
   feedback, and result describe one consistent outcome?
3. **Is hierarchy decisive and restrained?** Does the core feature receive the
   space it deserves while strong color, bold type, badges, alerts, and primary
   Buttons remain scarce?
4. **Could the interface do less, better?** Can an entry point, option, or state
   be removed, combined, or deferred without weakening the complete task?
5. **Is the structure exact?** Do peers share alignment spines, equal gaps stay
   equal to the rendered pixel, and scrollbars sit at the edge of their actual
   scrolling region?
6. **Does it follow the component system?** Do standard controls retain their
   geometry, states, keyboard behavior, and dismissal model, with appearance
   supplied by theme and scale tokens?
7. **Does it remain usable in every state and constraint?** Verify keyboard and
   focus behavior, empty/loading/failure/permission states, longer translations,
   zoom, minimum window size, and reduced motion.
8. **Has it been tested in a real window?** Complete the task with real
   components, copy, and representative content—not only an ideal screenshot.

Continue with [Coding Guides](./coding-guides.md) to translate these design
decisions into GPUI architecture and code.
