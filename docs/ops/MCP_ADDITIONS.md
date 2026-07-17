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
