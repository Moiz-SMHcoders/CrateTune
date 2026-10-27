; Inno Setup 6 script for the Windows installer (per-user install, no admin rights).
; Built by scripts/bundle-windows.ps1, which passes the identity from packaging/app.env:
;   iscc /DAppName=... /DAppId=... /DAppVersion=... /DAppSummary=... /DSourceExe=... /DOutputDir=... installer.iss
; (An MSI is possible with cargo-wix instead; see docs/USER_GUIDE.md → Install.)

#ifndef AppName
  #define AppName "Music"
#endif
#ifndef AppId
  #define AppId "music-macos"
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef AppSummary
  #define AppSummary ""
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\music-gpui.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\target\bundle"
#endif

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppComments={#AppSummary}
AppPublisher={#AppName} contributors
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#AppId}-{#AppVersion}-windows-x64-setup
SetupIconFile=app.ico
UninstallDisplayIcon={app}\{#AppId}.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppId}.exe"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppId}.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppId}.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppId}.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
