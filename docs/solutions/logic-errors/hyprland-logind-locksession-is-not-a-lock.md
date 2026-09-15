---
title: Hyprland logind LockSession success is not a screen lock
date: 2026-08-29
category: logic-errors
module: openlogi-inject
problem_type: logic_error
component: tooling
symptoms:
  - LockScreen binding logs success via logind but the Hyprland session stays unlocked
  - omarchy-system-lock never runs after LockSession returns OK
root_cause: logic_error
resolution_type: code_fix
severity: high
tags: [hyprland, logind, lock-screen, linux-inject, omarchy]
---

# Hyprland logind LockSession success is not a screen lock

## Problem

On Hyprland/Omarchy, treating a successful logind `LockSession` D-Bus call as "the screen is locked" skips the compositor lock helper. The call can return OK without hyprlock (or `omarchy-system-lock`) ever running.

## Symptoms

- Debug log `LockScreen via logind` with no lock UI
- Super+L is wrong on Omarchy (workspace layout toggle), so the GNOME/KDE fallback must not run either

## What Didn't Work

- Sharing GNOME/KDE's logind-first path on the Hyprland arm (plan originally required logind first). D-Bus success is not the same as a listener locking the session.

## Solution

Hyprland `LockScreen` calls `omarchy-system-lock`, then Super+Ctrl+L. It does not call `try_logind_lock`. Generic Linux lock still uses logind then Super+L. Pending in #1162.

## Why This Works

`try_logind_lock` returns true on any successful `LockSession` reply (`crates/openlogi-inject/src/inject/linux.rs`). GNOME/KDE listen for that signal; Hyprland typically does not. Early-return therefore looks successful and never reaches the Omarchy helper.

## Prevention

- Do not treat compositor-agnostic D-Bus OK as "the DE did the user-visible action" unless that DE is known to subscribe.
- Keep a unit test that Hyprland lock's helper is `omarchy-system-lock`, not Super+L.

## Related Issues

- Related: #1042 (same Linux native skip class on GNOME, different DE mapping)
- PR: #1162
