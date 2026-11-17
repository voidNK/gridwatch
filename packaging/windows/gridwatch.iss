; Inno Setup script for the Windows installer.
; Compile with:  ISCC /DVersion=0.8.4 /DProgram=..\..\target\release\gridwatch.exe gridwatch.iss
;
; gridwatch is installed for the current user only, without administrator rights:
; the built-in updater replaces gridwatch.exe and must be allowed to write there.

#ifndef Version
  #error Pass the version: /DVersion=x.y.z
#endif
#ifndef Program
  #define Program "..\..\target\release\gridwatch.exe"
#endif

[Setup]
AppId={{6F3B1C6E-52D7-4E0B-9B55-7A1D2F0C8E41}
AppName=gridwatch
AppVersion={#Version}
AppVerName=gridwatch {#Version}
AppPublisher=voidNK
AppPublisherURL=https://github.com/voidNK/gridwatch
AppSupportURL=https://github.com/voidNK/gridwatch/issues
AppUpdatesURL=https://github.com/voidNK/gridwatch/releases
DefaultDirName={autopf}\gridwatch
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=..\icon.ico
UninstallDisplayIcon={app}\gridwatch.exe
OutputDir=..\..\dist
OutputBaseFilename=gridwatch-v{#Version}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "Create a shortcut on the desktop"; Flags: unchecked
Name: "autostart"; Description: "Start gridwatch when I sign in"; Flags: unchecked

[Files]
Source: "{#Program}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\gridwatch"; Filename: "{app}\gridwatch.exe"
Name: "{autodesktop}\gridwatch"; Filename: "{app}\gridwatch.exe"; Tasks: desktopicon
Name: "{userstartup}\gridwatch"; Filename: "{app}\gridwatch.exe"; Tasks: autostart

[Run]
Filename: "{app}\gridwatch.exe"; Description: "Start gridwatch"; Flags: nowait postinstall skipifsilent
