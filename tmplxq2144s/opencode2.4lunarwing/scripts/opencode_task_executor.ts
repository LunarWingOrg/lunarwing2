
function sessionPermissions() {
  return [
    { action: "shell", resource: "*", effect: "allow" },
    { action: "shell", resource: "*opencode service *", effect: "deny" },
    { action: "shell", resource: "*opencode pair *", effect: "deny" },
    { action: "shell", resource: "*opencode serve *", effect: "deny" },
  ]
}
