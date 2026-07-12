# Consolidated: bare remote HEAD mismatch

> **Status: STILL-OPEN.** See
> [`BUG-ssh-git-ref-and-remote-head.md`](BUG-ssh-git-ref-and-remote-head.md#2-bare-remote-head-mismatch).

The canonical report corrects the old "clone fails" claim: Git exits 0 but
leaves an empty/unborn checkout.
