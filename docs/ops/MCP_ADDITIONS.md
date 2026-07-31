19. [x] MCP additions: ( please reference the following branch for a hint on getting started, old failed implementations from previous opencode/codex workers: faility/failed-partial-old-item-3-20260711-0601 AND slopmcp1/codex/upgrade/v2.0.0.0 ) - There are two sections below. You must read BOTH of them (as well as reference the old failed implementation from previous failed opencode worker). Once you have, follow the following instructions: One of the sections is `Recommended List — Pick ONE, implement it, check off item 18` Pick ONE from the `Recommended List — Pick ONE, implement it, check off item 18` list below, implement it, check off this box. Two Informational Sections following this sentence:
    <details>                                                                                                                                
    <summary><b>DONE — First-class host-local stdio MCP installation</b></summary>                                                           
    Implemented first-class host-local stdio MCP installation across LunarWing:                                                              
    - Added typed stdio MCP registry manifests with command, structured args, and non-secret env.                                            
    - Added a shared validated ExtensionManager::install_mcp_config persistence path.                                                        
    - Extended conversational tool_install and the web API to accept stdio configuration.                                                    
    - Added HTTP/stdio controls to the web MCP settings UI.                                                                                  
    - Added stdio support to lunarwing registry list/info/install, including --force handling.                                               
    - Exposed transport and command metadata when listing installed MCP servers.                                                             
    - Treated stdio servers as requiring no OAuth authentication.                                                                            
    - Added validation for empty commands, NUL bytes, and invalid environment names/values.                                                  
    - Ensured removal stops the managed child process before deleting its configuration.                                                     
    - Preserved existing HTTP MCP and registry precedence behavior.                                                                          
    - Kept installation separate from execution: installation stores configuration; activation starts the process and discovers tools.       
    - Updated the architecture proposal, historical supergateway note, and MCP documentation with neutral local-files examples.              
    Worker-local execution, automatic npm/pip installation, secret injection through stdio environment variables, and gateway changes were intentionally excluded. All targeted tests, formatting, JavaScript syntax checks, and cargo check passed.                                      
    </details>                                                                                                                               
    <details>                                                                                                                                
    <summary><b>Recommended List — Registry validation selected and completed</b></summary>
    Recommended Next:                                                                                                                        
    MCP deactivate/re-enable                                                                                                                 
       - Stop the child, unregister its tools, and preserve configuration.                                                                   
       - Persist enabled = false so restart does not relaunch it.                                                                            
       - Add tool_deactivate, API, and web controls.                                                                                         
       - This completes the lifecycle without involving WASM or workers.                                                                     
       Diagnostics and command preflight                                                                                                     
       - Extend doctor/status with transport, enabled state, and executable availability. 
    </details>

## v2.0.4 continuation (2026-07-31)

Goal 20 remains open. `rarity/kimi-19-20260730-1801` was reviewed as untrusted
reference and must not be cherry-picked wholesale:

- Its crash supervisor restarts a child but leaves `McpClient` and registered tool
  wrappers bound to the dead transport. Disable/shutdown races can also resurrect
  the process.
- Its automatic npm/pip path runs unverified package lifecycle code under the
  LunarWing service account without provenance, pinning, sandboxing, or rollback.
- Its resolved secret values are retained in process-manager restart config.

Completed safe slice:

- Stdio config supports `env_secrets` mappings from environment variable to
  owner-scoped LunarWing secret name. Only names are persisted.
- CLI uses `--secret-env KEY=SECRET_NAME`; web and conversational install/update
  requests use `env_secrets` objects.
- Activation resolves values for the activating owner and fails closed before
  spawn when the store or secret is unavailable.
- Temporary parent-side values use `SecretString`; no resolved values are retained
  for restart. Secret-bearing installs and all conversational MCP updates always
  require approval.
- Plain and secret mappings for the same environment variable are rejected.
- Stdio children receive an allowlisted compatibility environment plus explicit
  config instead of inheriting daemon credentials. Raw child stderr is suppressed
  when secret references are present.
- MCP update paths preserve forced-approval policy. `mcp test --user` now resolves
  secret references for that user and explicitly cleans up its runtime.
- Removed unused process-only `try_restart`; correct recovery belongs at
  `ExtensionManager` level and must recreate/initialize the client, rediscover
  tools, replace wrappers atomically, publish accurate status, and honor lifecycle
  cancellation generations.

Still deferred: automatic package installation pending provenance/sandbox policy,
safe automatic crash recovery, worker-local MCP, general runtime adapters, and MCP
gateway integration.
