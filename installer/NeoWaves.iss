; NeoWaves Inno Setup script
; Build: ISCC.exe installer\NeoWaves.iss

#define MyAppName "NeoWaves Audio List Editor"
#define MyAppShort "NeoWaves"
#define MyAppVersion "0.1.0"
#define MyAppExeName "neowaves.exe"

[Setup]
AppId={{8E0A3D0A-6A1B-4E2E-8C5A-2D6D9A6A0A11}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=NeoWaves
DefaultDirName={commonappdata}\{#MyAppShort}
DefaultGroupName={#MyAppShort}
OutputDir=..\target\release
OutputBaseFilename={#MyAppShort}-Setup-{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
Uninstallable=yes
PrivilegesRequired=admin

; Setup icon
SetupIconFile=..\icons\icon.ico

; Uninstall icon (Control Panel)
UninstallDisplayIcon={app}\{#MyAppExeName}

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\commands\*"; DestDir: "{app}\commands"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\icons\icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppShort}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{commondesktop}\{#MyAppShort}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; GroupDescription: "Additional icons:"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Run {#MyAppShort}"; Flags: nowait postinstall skipifsilent
