# Pebble Submodule

`pebble/` is the `nanogpt-community/pebble` Git submodule declared in the
repository-root `.gitmodules`. Initialize the pinned revision after cloning:

```bash
git submodule update --init --recursive pebble
```

`pebble4lunarwing/` is LunarWing's external-worker bridge and is a normal
tracked directory, not the submodule itself. Update the submodule pointer only
as a reviewed dependency change.
