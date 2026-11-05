; "Send to" in Explorer's context menu shares files into a conversation (#208). The folder belongs to
; the user running the installer. The MSI has no such entry: `teamcraft-desktop --share <paths>` works
; there, and a shortcut can be added by hand (README.md).
!macro NSIS_HOOK_POSTINSTALL
  CreateShortCut "$SENDTO\TeamCraft.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "--share --"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$SENDTO\TeamCraft.lnk"
!macroend
