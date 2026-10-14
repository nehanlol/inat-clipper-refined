; Script generated for INAT - Video & Audio Downloader & Clipper
#define MyAppName "INAT"
#define MyAppVersion "1.0.0"
#define MyAppPublisher "INAT"
#define MyAppURL "https://github.com/stsuprim/inat-clipper"
#define MyAppExeName "INAT.exe"

[Setup]
AppId={{5E7989C7-93BC-4D78-9B4B-6B8E85DF4B1C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
UninstallDisplayIcon={app}\{#MyAppExeName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=INAT_Setup_v{#MyAppVersion}
SetupIconFile=..\assets\inat.ico
SolidCompression=yes
Compression=lzma2/ultra64
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DisableProgramGroupPage=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "taskbarpin"; Description: "Pin to Taskbar"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\dist\inat-gui.exe"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion
Source: "..\assets\inat.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "pin_taskbar.ps1"; DestDir: "{tmp}"; Flags: ignoreversion deleteafterinstall

[Icons]
Name: "{autoprograms}\{#MyAppName}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\inat.ico"
Name: "{autoprograms}\{#MyAppName}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\inat.ico"; Tasks: desktopicon
; Taskbar User Pinned shortcut
Name: "{userappdata}\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\inat.ico"; Tasks: taskbarpin

[Run]
Filename: "powershell.exe"; Parameters: "-ExecutionPolicy Bypass -WindowStyle Hidden -File ""{tmp}\pin_taskbar.ps1"" ""{app}\{#MyAppExeName}"""; Flags: runhidden; Tasks: taskbarpin
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
