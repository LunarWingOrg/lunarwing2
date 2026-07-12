# Questions

Now let me catalog the changes the dev loop script needs for v2/Codeberg. Looking at lunarw
ing-item.sh, here's what I see:                                                                
                                                                                               
    CURRENT → NEEDED                                                                           
    1. REPO path: /home/rarity/lunarwing → /home/rarity/lunarwing_v2                           
    2. Auth: GITHUB_TOKEN + git remote set-url https://x-access-token:... → SSH key (GIT_SSH_CO
MMAND="ssh -i ~/.ssh/cb_lw2_ed25519" or SSH config)                                            
    3. PR creation: gh pr create (GitHub API) → Codeberg/Forgejo API via curl                  
    4. PR listing: gh pr list → Forgejo API via curl                                           
    5. Base branch: staging → master (v2's default)                                            
    6. Goals file: AGENT_GOALS_1.1.9.md → whatever v2 uses (it's there but might be placeholder
)                                             