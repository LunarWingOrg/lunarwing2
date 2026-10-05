10. [ ] Verify below Gentoo Issue, then write up a document in docs/ops detailing the status: is each piece verifiable? what outstanding issues remain? which points have already been addressed?                                                                                         
    <details>                                                                                                                                
    <summary><b>INFO DUMP — Gentoo WeeChat relay-api</b></summary>                                                                           
    Issue: -relay-api is disabled (the API protocol needs cJSON). emerge needs cjson.                                                        
    Fix: (commands)                                                                                                                          
    echo "net-irc/weechat relay-api" | sudo tee -a /etc/portage/package.use/weechat                                                          
    sudo emerge --oneshot --changed-use net-irc/weechat                                                                                      
    Document the accuracy of this issue.                                                                                                     
    </details>    
