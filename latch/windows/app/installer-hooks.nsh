; Plow Latch's installer (Tauri's NSIS bundle): what it does beyond copying
; the window and the latch beside it.

!macro NSIS_HOOK_PREUNINSTALL
  ; Latch's host stops, and no longer starts when this user logs in (the
  ; Run entry the window's "Start when you log in" makes).
  nsExec::Exec '"$INSTDIR\latch.exe" stop'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Plow Latch"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Google's tool, the pinned gog, installed now rather than at first use
  ; (`plugins enable google` installs it too when it's missing).
  nsExec::Exec '"$INSTDIR\latch.exe" install-gog'
!macroend
