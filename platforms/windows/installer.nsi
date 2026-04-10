; Dimmy NSIS Installer — currentUser mode (no admin/UAC)
; Built by CI, installs to %LOCALAPPDATA%\Dimmy

!include "MUI2.nsh"
!include "FileFunc.nsh"

; ── Metadata ─────────────────────────────────────────────────────
!define APP_NAME "Dimmy"
!define APP_EXE "Dimmy.Windows.exe"
!define APP_PUBLISHER "KonradDallaOrg"
!define APP_URL "https://github.com/KonradDallaOrg/dimmy"

; Version injected by CI via /DAPP_VERSION=x.y.z
!ifndef APP_VERSION
  !define APP_VERSION "0.0.0"
!endif

Name "${APP_NAME} ${APP_VERSION}"
OutFile "Dimmy-setup-${APP_VERSION}-x64.exe"
InstallDir "$LOCALAPPDATA\${APP_NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show

; ── Icon ─────────────────────────────────────────────────────────
!define MUI_ICON "Dimmy.Windows\Assets\dimmy.ico"
!define MUI_UNICON "Dimmy.Windows\Assets\dimmy.ico"

; ── Pages ────────────────────────────────────────────────────────
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Italian"

; ── Prerequisites: .NET 8 Desktop Runtime + Windows App Runtime ──
!define DOTNET_URL "https://download.visualstudio.microsoft.com/download/pr/27bcdd70-ce64-4049-ba24-2b14e81d3a62/3dde1f825d2cce14a76e21b00d0aefc6/windowsdesktop-runtime-8.0.16-win-x64.exe"
!define DOTNET_INSTALLER "dotnet-runtime-8-desktop-x64.exe"
!define WINAPPRT_URL "https://aka.ms/windowsappsdk/1.6/1.6.241114003/windowsappruntimeinstall-x64.exe"
!define WINAPPRT_INSTALLER "windowsappruntimeinstall-x64.exe"

Function CheckDotNet
  IfFileExists "$PROGRAMFILES64\dotnet\shared\Microsoft.WindowsDesktop.App\8.*" dotnet_found
  IfFileExists "$PROGRAMFILES\dotnet\shared\Microsoft.WindowsDesktop.App\8.*" dotnet_found

  DetailPrint "Downloading .NET 8 Desktop Runtime..."
  NSISdl::download "${DOTNET_URL}" "$TEMP\${DOTNET_INSTALLER}"
  Pop $0
  StrCmp $0 "success" +3
    MessageBox MB_OK|MB_ICONEXCLAMATION "Failed to download .NET 8 Runtime. Please install it manually from dotnet.microsoft.com"
    Goto dotnet_done

  DetailPrint "Installing .NET 8 Desktop Runtime..."
  nsExec::ExecToLog '"$TEMP\${DOTNET_INSTALLER}" /install /quiet /norestart'
  Delete "$TEMP\${DOTNET_INSTALLER}"
  DetailPrint ".NET 8 Desktop Runtime installed."
  Goto dotnet_done

  dotnet_found:
    DetailPrint ".NET 8 Desktop Runtime: OK"
  dotnet_done:
FunctionEnd

Function CheckWinAppRuntime
  ; Check if Windows App Runtime 1.6 is registered
  EnumRegKey $0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" 0
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\{6BB3A607-B400-4513-9E7E-8E74A68E7F2B}" "DisplayName"
  StrCmp $0 "" 0 winapprt_found

  ; Also check via the DLL existence (more reliable)
  IfFileExists "$PROGRAMFILES\WindowsApps\Microsoft.WindowsAppRuntime.1.6*\Microsoft.WindowsAppRuntime.dll" winapprt_found

  DetailPrint "Downloading Windows App Runtime..."
  NSISdl::download "${WINAPPRT_URL}" "$TEMP\${WINAPPRT_INSTALLER}"
  Pop $0
  StrCmp $0 "success" +3
    MessageBox MB_OK|MB_ICONEXCLAMATION "Failed to download Windows App Runtime. The app will prompt you to install it on first launch."
    Goto winapprt_done

  DetailPrint "Installing Windows App Runtime..."
  nsExec::ExecToLog '"$TEMP\${WINAPPRT_INSTALLER}" --quiet'
  Delete "$TEMP\${WINAPPRT_INSTALLER}"
  DetailPrint "Windows App Runtime installed."
  Goto winapprt_done

  winapprt_found:
    DetailPrint "Windows App Runtime: OK"
  winapprt_done:
FunctionEnd

; ── Install section ──────────────────────────────────────────────
Section "Install"
  ; Install prerequisites silently if missing
  Call CheckDotNet
  Call CheckWinAppRuntime

  ; Kill running instance
  nsExec::ExecToLog 'taskkill /f /im ${APP_EXE}'

  ; Clean previous installation to avoid stale DLL conflicts
  RMDir /r "$INSTDIR"

  SetOutPath "$INSTDIR"

  ; Copy all files from the publish directory
  File /r "Dimmy.Windows\publish\*.*"

  ; Write uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Start menu shortcut
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\Uninstall.lnk" "$INSTDIR\uninstall.exe"

  ; Desktop shortcut
  CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0

  ; Registry: Add/Remove Programs
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "DisplayName" "${APP_NAME}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "Publisher" "${APP_PUBLISHER}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "URLInfoAbout" "${APP_URL}"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "DisplayVersion" "${APP_VERSION}"

  ; Compute installed size
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
    "EstimatedSize" "$0"
SectionEnd

; ── Uninstall section ────────────────────────────────────────────
Section "Uninstall"
  ; Kill running instance
  nsExec::ExecToLog 'taskkill /f /im ${APP_EXE}'

  ; Remove files
  RMDir /r "$INSTDIR"

  ; Remove shortcuts
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; Remove registry
  DeleteRegKey SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
SectionEnd
