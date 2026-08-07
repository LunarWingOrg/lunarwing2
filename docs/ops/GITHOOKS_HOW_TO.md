# Git Hook Setup

Run from the repository root. The installer sets this clone's
`core.hooksPath=.githooks`; it does not copy files into `.git/hooks/`.

```bash
./scripts/install-githooks.sh           # install once per clone
./scripts/install-githooks.sh --status  # inspect the active hooks path
./scripts/install-githooks.sh --remove  # restore Git's default hook path
```

The committed `pre-commit`, `commit-msg`, and `pre-push` hooks then run
automatically. Use Git's `--no-verify` only for an intentional, reviewed bypass.
