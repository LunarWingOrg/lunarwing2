# Issues

> **Current status (2026-07-20, rev `50c8f99`): PARTIAL.** systemd/OpenRC
> service rendering, enablement, and registry-backed adapter ports are
> implemented. Dependency handling remains warn-only; mt-admin does not install
> `aiohttp` automatically.

* Historical problem: starting the WeeChat WebSocket adapter required a manual,
  persistent tmux pane.

To make this WAY better, we can:

1. [x] Make the adapter into a systemd/OpenRC service.
2. [ ] Automate installation of `aiohttp` during multi-tenant setup. Current
   behavior warns when the dependency is missing.
3. [x] Enable the service from item 1 for the tenant.
4. [x] Manage the adapter's local HTTP port through mt-admin and the ports registry.
