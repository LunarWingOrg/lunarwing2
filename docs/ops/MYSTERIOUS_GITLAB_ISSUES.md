**Knowledge note updated.** 🜂

| Field | Value |
|-------|-------|
| **Topic** | `bug-007-gitlab-tool-additional-properties` |
| **Status** | Active |
| **Scope** | Shared |
| **Replace** | true (dwarfstar's version) |

**Updated with:**
- `list_issues` → requires `project` ✅
- `create_issue` → requires `project` ✅
- `update_issue` → requires `project` + `iid` ✅
- Parameter is `iid`, not `issue_iid`
- All write actions confirmed operational
- Original serialization hypothesis retracted

BUG-007 documentation is now accurate. What's next? 🜂