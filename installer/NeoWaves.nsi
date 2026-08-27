; NeoWaves NSIS installer script
;
; Build: makensis /DAPP_VERSION=1.2.3 /DOUT_FILE=<abs path> installer\NeoWaves.nsi
; Driver: commands\build_installer.ps1
;
; NSIS is used instead of Inno Setup because NSIS is under the zlib/libpng
; licence, which permits commercial use outright -- no purchase is requested and
; no acknowledgement is required. Inno Setup's publisher asks every commercial
; user to buy a licence, which made every installer this project produced a
; "non-commercial / test only" artefact. See assets/licenses/texts/
; NEOWAVES-COMPLIANCE.txt, section INSTALLER BUILD TOOL.
;
; Source paths below use forward slashes on purpose: Windows accepts them and it
; lets makensis build this script on a POSIX host, which is how it gets checked
; outside a Windows runner.

Unicode true

;--------------------------------------------------------------------------
; Identity
;--------------------------------------------------------------------------

!define APP_NAME      "NeoWaves Audio List Editor"
!define APP_SHORT     "NeoWaves"
!define APP_PUBLISHER "NeoWaves"
!define APP_EXE       "neowaves.exe"
!define WORKER_EXE    "neowaves_plugin_worker.exe"
!define GUI_WORKER_EXE "neowaves_plugin_gui_worker.exe"
!define APP_ASSOC     "NeoWaves.Audio"
!define APP_REGKEY    "Software\NeoWaves"
!define UNINST_KEY    "Software\Microsoft\Windows\CurrentVersion\Uninstall\NeoWaves"
; Written under each extension we take over, so uninstall can hand the
; extension back to whatever owned it before instead of leaving it orphaned.
!define ASSOC_BACKUP  "NeoWaves.Backup"
; Releases up to 0.20260827.0 were packaged with Inno Setup, which registered
; itself under its own AppId. Without this, upgrading leaves a second entry in
; Add/Remove Programs and an orphaned unins000.exe in the install directory.
;
; That .iss never set ArchitecturesInstallIn64BitMode, so Inno ran 32-bit: the
; install landed in Program Files (x86) and its uninstall key went to the
; WOW6432Node view. Both views are therefore searched, and an existing install
; is reused where it already lives rather than being duplicated under a
; 64-bit path.
!define LEGACY_INNO_KEY \
  "Software\Microsoft\Windows\CurrentVersion\Uninstall\{8E0A3D0A-6A1B-4E2E-8C5A-2D6D9A6A0A11}_is1"

;--------------------------------------------------------------------------
; Inputs (all overridable with makensis /D..., all defaulted so the script
; compiles on its own)
;--------------------------------------------------------------------------

!ifndef APP_VERSION
  !define APP_VERSION "0.0.0"
!endif
!ifndef BUILD_ID
  !define BUILD_ID ""
!endif
!ifndef PRIVILEGES
  !define PRIVILEGES "admin"
!endif
; Where the compiled binaries and the LAME DLL are.
!ifndef SRC_DIR
  !define SRC_DIR "../target/release"
!endif
; Repository root, for LICENSE / icons / commands / notices.
!ifndef REPO_DIR
  !define REPO_DIR ".."
!endif

!if "${BUILD_ID}" == ""
  !define OUT_SUFFIX ""
!else
  !define OUT_SUFFIX "-${BUILD_ID}"
!endif

!ifndef OUT_FILE
  !define OUT_FILE "../target/installer/${APP_SHORT}-Setup-${APP_VERSION}${OUT_SUFFIX}.exe"
!endif

;--------------------------------------------------------------------------
; Privilege level. RequestExecutionLevel is a compile-time constant in NSIS,
; so the per-machine / per-user split is decided here rather than at run time.
; This mirrors Inno's PrivilegesRequired, which build_installer.ps1 passes
; through unchanged.
;--------------------------------------------------------------------------

!if "${PRIVILEGES}" == "lowest"
  RequestExecutionLevel user
  !define DEFAULT_INSTDIR "$LOCALAPPDATA\Programs\${APP_SHORT}"
  InstallDir "${DEFAULT_INSTDIR}"
  !define MULTIUSER_CONTEXT "current"
  !define APP_ROOT     HKCU
  !define CLASSES_ROOT HKCU
!else if "${PRIVILEGES}" == "poweruser"
  RequestExecutionLevel highest
  !define DEFAULT_INSTDIR "$PROGRAMFILES64\${APP_SHORT}"
  InstallDir "${DEFAULT_INSTDIR}"
  !define MULTIUSER_CONTEXT "all"
  !define APP_ROOT     HKLM
  !define CLASSES_ROOT HKLM
!else
  RequestExecutionLevel admin
  !define DEFAULT_INSTDIR "$PROGRAMFILES64\${APP_SHORT}"
  InstallDir "${DEFAULT_INSTDIR}"
  !define MULTIUSER_CONTEXT "all"
  !define APP_ROOT     HKLM
  !define CLASSES_ROOT HKLM
!endif

; Inno wrote associations to HKCR, which resolves to HKLM\Software\Classes for
; an elevated install. Naming the hive explicitly keeps a per-user install out
; of the machine-wide hive instead of relying on registry virtualisation.
!define CLASSES "Software\Classes"

;--------------------------------------------------------------------------
; Output
;--------------------------------------------------------------------------

Name "${APP_NAME}"
Caption "${APP_NAME} ${APP_VERSION}"
BrandingText "${APP_SHORT} ${APP_VERSION}"
OutFile "${OUT_FILE}"
InstallDirRegKey ${APP_ROOT} "${APP_REGKEY}" "InstallLocation"
SetCompressor /SOLID lzma
XPStyle on

; VIProductVersion caps each field at 65535, and this project's versions are
; date-stamped (0.20260827.0), so the real one only fits in the string keys.
VIProductVersion "0.0.0.0"
VIAddVersionKey "ProductName"     "${APP_NAME}"
VIAddVersionKey "CompanyName"     "${APP_PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey "FileVersion"     "${APP_VERSION}"
VIAddVersionKey "ProductVersion"  "${APP_VERSION}"
VIAddVersionKey "LegalCopyright"  "Copyright (c) 2026 zukky"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"
!include "FileFunc.nsh"

; FileFunc's macros have to be instantiated before ${GetSize} can be used.
!insertmacro GetSize

;--------------------------------------------------------------------------
; Pages
;--------------------------------------------------------------------------

!define MUI_ICON   "${REPO_DIR}/icons/icon.ico"
!define MUI_UNICON "${REPO_DIR}/icons/icon.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${REPO_DIR}/LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Run ${APP_SHORT}"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchAsOriginalUser
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

;--------------------------------------------------------------------------
; Running-instance handling
;
; Inno had CloseApplications + CloseApplicationsFilter. Windows locks a running
; executable against writes, so opening each one for append is a direct test of
; the thing that actually matters: can this install overwrite the file. When one
; is locked, `taskkill` without /F posts WM_CLOSE -- the same graceful close
; Inno performed -- and the check runs again.
;--------------------------------------------------------------------------

Var RunningExe

; Sets $RunningExe to the name of the first locked executable, or "" if every
; one of them is free.
!macro CheckLocked EXE
  ${If} $RunningExe == ""
  ${AndIf} ${FileExists} "$INSTDIR\${EXE}"
    ClearErrors
    FileOpen $0 "$INSTDIR\${EXE}" a
    ${If} ${Errors}
      StrCpy $RunningExe "${EXE}"
    ${Else}
      FileClose $0
    ${EndIf}
  ${EndIf}
!macroend

Function DetectRunningNeoWaves
  Push $0
  StrCpy $RunningExe ""
  !insertmacro CheckLocked "${APP_EXE}"
  !insertmacro CheckLocked "${WORKER_EXE}"
  !insertmacro CheckLocked "${GUI_WORKER_EXE}"
  Pop $0
FunctionEnd

Function CloseRunningNeoWaves
  Push $0
  Push $1
  StrCpy $0 0

  close_loop:
    Call DetectRunningNeoWaves
    StrCmp $RunningExe "" close_done
    ; Two graceful rounds is enough; after that it is the user's call.
    IntCmp $0 2 close_ask 0 close_ask

    DetailPrint "Closing $RunningExe..."
    nsExec::Exec 'taskkill /IM "${APP_EXE}"'
    Pop $1
    nsExec::Exec 'taskkill /IM "${GUI_WORKER_EXE}"'
    Pop $1
    nsExec::Exec 'taskkill /IM "${WORKER_EXE}"'
    Pop $1
    Sleep 1500
    IntOp $0 $0 + 1
    Goto close_loop

  close_ask:
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION \
      "$RunningExe is still running.$\n$\nClose ${APP_SHORT} and press Retry, or press Cancel to stop the installation." \
      /SD IDCANCEL IDRETRY close_retry
    Abort "Installation cancelled: $RunningExe is in use."

  close_retry:
    StrCpy $0 0
    Goto close_loop

  close_done:
  Pop $1
  Pop $0
FunctionEnd

;--------------------------------------------------------------------------
; File association helpers
;--------------------------------------------------------------------------

; Reads a value from the Inno-era uninstall key, checking the 32-bit view first
; because that is where the shipped installer actually wrote it.
!macro ReadLegacyInno OUT VALUE
  SetRegView 32
  ReadRegStr ${OUT} HKLM "${LEGACY_INNO_KEY}" "${VALUE}"
  SetRegView 64
  ${If} ${OUT} == ""
    ReadRegStr ${OUT} HKLM "${LEGACY_INNO_KEY}" "${VALUE}"
  ${EndIf}
!macroend

!macro DeleteLegacyInno
  SetRegView 32
  DeleteRegKey HKLM "${LEGACY_INNO_KEY}"
  SetRegView 64
  DeleteRegKey HKLM "${LEGACY_INNO_KEY}"
!macroend

!macro RegisterExtension EXT
  ; Remember the previous owner exactly once, so a reinstall does not record
  ; NeoWaves as the thing to restore later.
  ReadRegStr $0 ${CLASSES_ROOT} "${CLASSES}\${EXT}" ""
  ReadRegStr $1 ${CLASSES_ROOT} "${CLASSES}\${EXT}" "${ASSOC_BACKUP}"
  ${If} $1 == ""
  ${AndIf} $0 != "${APP_ASSOC}"
    WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${EXT}" "${ASSOC_BACKUP}" "$0"
  ${EndIf}

  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${EXT}" "" "${APP_ASSOC}"
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${EXT}\OpenWithProgids" "${APP_ASSOC}" ""
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\Applications\${APP_EXE}\SupportedTypes" "${EXT}" ""
!macroend

!macro UnregisterExtension EXT
  ReadRegStr $0 ${CLASSES_ROOT} "${CLASSES}\${EXT}" ""
  ${If} $0 == "${APP_ASSOC}"
    ReadRegStr $1 ${CLASSES_ROOT} "${CLASSES}\${EXT}" "${ASSOC_BACKUP}"
    ${If} $1 == ""
      DeleteRegValue ${CLASSES_ROOT} "${CLASSES}\${EXT}" ""
    ${Else}
      WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${EXT}" "" "$1"
    ${EndIf}
  ${EndIf}
  DeleteRegValue ${CLASSES_ROOT} "${CLASSES}\${EXT}" "${ASSOC_BACKUP}"
  DeleteRegValue ${CLASSES_ROOT} "${CLASSES}\${EXT}\OpenWithProgids" "${APP_ASSOC}"
  DeleteRegKey /ifempty ${CLASSES_ROOT} "${CLASSES}\${EXT}\OpenWithProgids"
!macroend

; Every extension NeoWaves offers to open. Kept in one macro so the install and
; uninstall halves cannot drift apart.
!macro ForEachExtension OP
  !insertmacro ${OP} ".wav"
  !insertmacro ${OP} ".aiff"
  !insertmacro ${OP} ".aif"
  !insertmacro ${OP} ".flac"
  !insertmacro ${OP} ".mp3"
  !insertmacro ${OP} ".m4a"
  !insertmacro ${OP} ".ogg"
  !insertmacro ${OP} ".mp4"
  !insertmacro ${OP} ".mov"
  !insertmacro ${OP} ".m4v"
  !insertmacro ${OP} ".3gp"
  !insertmacro ${OP} ".3g2"
  !insertmacro ${OP} ".nwsess"
!macroend

;--------------------------------------------------------------------------
; Install
;--------------------------------------------------------------------------

Section "!${APP_SHORT}" SecCore
  SectionIn RO

  Call CloseRunningNeoWaves

  SetOutPath "$INSTDIR"
  SetOverwrite on

  File "${SRC_DIR}/${APP_EXE}"
  File "${SRC_DIR}/${WORKER_EXE}"
  File "${SRC_DIR}/${GUI_WORKER_EXE}"

  ; LAME is deliberately a separate, replaceable DLL (LGPL-2.0 section 6(b)).
  ; It must never be folded into the executable.
  File "${SRC_DIR}/libmp3lame.dll"

  ; ONNX Runtime is normally static. Keep these as a safety net if its
  ; packaging changes in a future ort release.
  File /nonfatal "${SRC_DIR}/onnxruntime*.dll"
  File /nonfatal "${SRC_DIR}/onnxruntime_providers*.dll"

  File "${REPO_DIR}/LICENSE"
  File "${REPO_DIR}/assets/licenses/THIRD_PARTY_NOTICES.txt"
  File "${REPO_DIR}/icons/icon.ico"

  SetOutPath "$INSTDIR\commands"
  File /r "${REPO_DIR}/commands/*"
  SetOutPath "$INSTDIR"

  ; Start menu entry (Inno created this unconditionally).
  CreateDirectory "$SMPROGRAMS\${APP_SHORT}"
  CreateShortcut "$SMPROGRAMS\${APP_SHORT}\${APP_SHORT}.lnk" \
    "$INSTDIR\${APP_EXE}" "" "$INSTDIR\icon.ico"

  WriteRegStr ${APP_ROOT} "${APP_REGKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr ${APP_ROOT} "${APP_REGKEY}" "Version" "${APP_VERSION}"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "DisplayVersion"  "${APP_VERSION}"
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "Publisher"       "${APP_PUBLISHER}"
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\${APP_EXE}"
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   ${APP_ROOT} "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD ${APP_ROOT} "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD ${APP_ROOT} "${UNINST_KEY}" "NoRepair" 1

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD ${APP_ROOT} "${UNINST_KEY}" "EstimatedSize" "$0"

  ; This install now owns the directory, so retire the Inno-era registration
  ; rather than leaving a second Add/Remove Programs entry pointing at an
  ; uninstaller whose file list is already stale.
  !insertmacro ReadLegacyInno $0 "UninstallString"
  ${If} $0 != ""
    DetailPrint "Removing the previous Inno Setup registration..."
    !insertmacro DeleteLegacyInno
    Delete "$INSTDIR\unins000.exe"
    Delete "$INSTDIR\unins000.dat"
  ${EndIf}
SectionEnd

Section "Create a desktop icon" SecDesktopIcon
  CreateShortcut "$DESKTOP\${APP_SHORT}.lnk" \
    "$INSTDIR\${APP_EXE}" "" "$INSTDIR\icon.ico"
SectionEnd

; Unchecked by default, matching Inno's `Flags: unchecked` on the assoc task.
Section /o "Associate audio and video files with ${APP_SHORT}" SecFileAssoc
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${APP_ASSOC}" "" "${APP_NAME}"
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${APP_ASSOC}\DefaultIcon" "" "$INSTDIR\icon.ico"
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\${APP_ASSOC}\shell\open\command" "" '"$INSTDIR\${APP_EXE}" "%1"'

  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\Applications\${APP_EXE}" "" "${APP_NAME}"
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\Applications\${APP_EXE}\DefaultIcon" "" "$INSTDIR\icon.ico"
  WriteRegStr ${CLASSES_ROOT} "${CLASSES}\Applications\${APP_EXE}\shell\open\command" "" '"$INSTDIR\${APP_EXE}" "%1"'

  !insertmacro ForEachExtension RegisterExtension

  WriteRegDWORD ${APP_ROOT} "${APP_REGKEY}" "FileAssociations" 1
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'
SectionEnd

LangString DESC_SecCore        ${LANG_ENGLISH} "${APP_NAME} and the licence notices for everything it ships."
LangString DESC_SecDesktopIcon ${LANG_ENGLISH} "Put a ${APP_SHORT} shortcut on the desktop."
LangString DESC_SecFileAssoc   ${LANG_ENGLISH} "Open .wav / .aiff / .flac / .mp3 / .m4a / .ogg / .mp4 / .mov / .m4v / .3gp / .3g2 / .nwsess with ${APP_SHORT} by default."

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}        $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktopIcon} $(DESC_SecDesktopIcon)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecFileAssoc}   $(DESC_SecFileAssoc)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

;--------------------------------------------------------------------------
; Init
;--------------------------------------------------------------------------

Function .onInit
  SetShellVarContext ${MULTIUSER_CONTEXT}
  ; HKCU\Software is not redirected, so this is simply the right view for the
  ; per-machine case and harmless for the per-user one.
  SetRegView 64

  ; Reuse the previous install directory the way Inno's UsePreviousAppDir did,
  ; but never over the top of an explicit /D= on the command line. NSIS applies
  ; /D= before .onInit runs, so "still the compiled-in default" is what
  ; distinguishes the two.
  ${If} $INSTDIR == "${DEFAULT_INSTDIR}"
    ReadRegStr $0 ${APP_ROOT} "${APP_REGKEY}" "InstallLocation"
    ${If} $0 == ""
      ; Nothing of ours yet: this may be an upgrade over an Inno-built install,
      ; which is the only other thing that knows where NeoWaves lives.
      !insertmacro ReadLegacyInno $0 "InstallLocation"
    ${EndIf}
    ${If} $0 != ""
      StrCpy $INSTDIR "$0"
    ${EndIf}
  ${EndIf}

  ; And re-tick the association task if it was chosen last time, matching
  ; UsePreviousTasks.
  ReadRegDWORD $0 ${APP_ROOT} "${APP_REGKEY}" "FileAssociations"
  ${If} $0 = 1
    !insertmacro SelectSection ${SecFileAssoc}
  ${EndIf}
FunctionEnd

; Setup runs elevated, so a plain Exec would hand NeoWaves an administrator
; token and send it looking for the per-user HuggingFace cache
; (%USERPROFILE%\.cache\huggingface\hub) in the wrong profile. Launching through
; the already-running shell drops back to the interactive user, which is what
; Inno's `runasoriginaluser` flag did.
Function LaunchAsOriginalUser
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\${APP_EXE}"'
FunctionEnd

;--------------------------------------------------------------------------
; Uninstall
;--------------------------------------------------------------------------

Function un.onInit
  SetShellVarContext ${MULTIUSER_CONTEXT}
  SetRegView 64
FunctionEnd

Section "Uninstall"
  !insertmacro ForEachExtension UnregisterExtension
  DeleteRegKey ${CLASSES_ROOT} "${CLASSES}\${APP_ASSOC}"
  DeleteRegKey ${CLASSES_ROOT} "${CLASSES}\Applications\${APP_EXE}"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'

  Delete "$DESKTOP\${APP_SHORT}.lnk"
  Delete "$SMPROGRAMS\${APP_SHORT}\${APP_SHORT}.lnk"
  RMDir  "$SMPROGRAMS\${APP_SHORT}"

  RMDir /r "$INSTDIR\commands"

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\${WORKER_EXE}"
  Delete "$INSTDIR\${GUI_WORKER_EXE}"
  Delete "$INSTDIR\libmp3lame.dll"
  Delete "$INSTDIR\onnxruntime*.dll"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.txt"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\Uninstall.exe"

  ; Not /r: anything the user put here is theirs, and the directory simply
  ; stays if it is not empty.
  RMDir "$INSTDIR"

  DeleteRegKey ${APP_ROOT} "${UNINST_KEY}"
  DeleteRegKey ${APP_ROOT} "${APP_REGKEY}"
SectionEnd
