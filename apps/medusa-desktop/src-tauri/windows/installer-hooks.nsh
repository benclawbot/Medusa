!include "LogicLib.nsh"

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Checking for Node.js 22 and npm"
  nsExec::ExecToStack 'cmd.exe /d /s /c "where node.exe >nul 2>nul && where npm.cmd >nul 2>nul"'
  Pop $0

  ${If} $0 != 0
    DetailPrint "Node.js/npm not found; installing Node.js 22 with WinGet"
    nsExec::ExecToLog 'winget.exe install --id OpenJS.NodeJS --exact --version 22.21.1 --source winget --accept-package-agreements --accept-source-agreements --silent --disable-interactivity'
    Pop $0
    ${If} $0 != 0
      MessageBox MB_ICONSTOP|MB_OK "Medusa Desktop could not install Node.js 22/npm with WinGet (exit code $0). Install Node.js 22 manually, then run the installer again."
      Abort
    ${EndIf}
  ${EndIf}

  IfFileExists "$PROGRAMFILES64\nodejs\npm.cmd" node_ready 0
  IfFileExists "$PROGRAMFILES\nodejs\npm.cmd" node_ready 0
  nsExec::ExecToStack 'cmd.exe /d /s /c "where npm.cmd >nul 2>nul"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Node.js was installed, but npm.cmd could not be found. Repair the Node.js 22 installation, then run the Medusa installer again."
    Abort
  ${EndIf}

  node_ready:
  DetailPrint "Node.js/npm dependency is available"
!macroend
