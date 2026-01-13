; NeoWaves Inno Setup script
; Build: ISCC.exe installer\NeoWaves.iss

#define MyAppName "NeoWaves Audio List Editor"
#define MyAppShort "NeoWaves"
#define MyAppVersion "0.1.0"
#define MyAppExeName "neowaves.exe"
#define MyAppAssoc "NeoWaves.Audio"

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
ChangesAssociations=yes

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
Name: "assoc"; Description: "Associate .wav/.mp3/.m4a with {#MyAppShort}"; GroupDescription: "File associations:"; Flags: unchecked

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Run {#MyAppShort}"; Flags: nowait postinstall skipifsilent

[Registry]
Root: HKCR; Subkey: ".wav"; ValueType: string; ValueName: ""; ValueData: "{#MyAppAssoc}"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCR; Subkey: ".mp3"; ValueType: string; ValueName: ""; ValueData: "{#MyAppAssoc}"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCR; Subkey: ".m4a"; ValueType: string; ValueName: ""; ValueData: "{#MyAppAssoc}"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCR; Subkey: "{#MyAppAssoc}"; ValueType: string; ValueName: ""; ValueData: "{#MyAppName}"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "{#MyAppAssoc}\\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\\icon.ico"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "{#MyAppAssoc}\\shell\\open\\command"; ValueType: string; ValueName: ""; ValueData: "\"{app}\\{#MyAppExeName}\" %*"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}"; ValueType: string; ValueName: ""; ValueData: "{#MyAppName}"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}\\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\\icon.ico"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}\\shell\\open\\command"; ValueType: string; ValueName: ""; ValueData: "\"{app}\\{#MyAppExeName}\" %*"; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}\\SupportedTypes"; ValueType: string; ValueName: ".wav"; ValueData: ""; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}\\SupportedTypes"; ValueType: string; ValueName: ".mp3"; ValueData: ""; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: "Applications\\{#MyAppExeName}\\SupportedTypes"; ValueType: string; ValueName: ".m4a"; ValueData: ""; Flags: uninsdeletekey; Tasks: assoc
Root: HKCR; Subkey: ".wav\\OpenWithProgids"; ValueType: string; ValueName: "{#MyAppAssoc}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCR; Subkey: ".mp3\\OpenWithProgids"; ValueType: string; ValueName: "{#MyAppAssoc}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKCR; Subkey: ".m4a\\OpenWithProgids"; ValueType: string; ValueName: "{#MyAppAssoc}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
