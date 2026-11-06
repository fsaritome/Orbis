; Orbis installer (Inno Setup 6). Built by scripts\package.ps1, which passes
; the version and checks that the release build and sdk\bin exist:
;   iscc /DAppVersion=0.1.0 installer\orbis.iss
; Installs per user (no admin prompt) into %LOCALAPPDATA%\Programs\Orbis.
; The Fujifilm SDK DLLs come from your local sdk\bin. The SDK agreement
; allows shipping them inside Orbis if users accept its obligations (the
; license page) and are told Fujifilm gives no support or warranty
; (installer\fujifilm-sdk-notice.txt). Update sdk\bin when Fujifilm ships
; bug fixes (agreement 3.1).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define Root ".."

[Setup]
AppId={{6B0C2E5D-6A1F-4C1B-9E0F-0F5E5C2EC4A6}
AppName=Orbis
AppVersion={#AppVersion}
AppVerName=Orbis {#AppVersion}
AppPublisher=fsaritome
AppPublisherURL=https://github.com/fsaritome/Orbis
AppSupportURL=https://github.com/fsaritome/Orbis/issues
DefaultDirName={autopf}\Orbis
DefaultGroupName=Orbis
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\target\installer
OutputBaseFilename=Orbis-Setup-{#AppVersion}
SetupIconFile={#Root}\docs\img\orbis.ico
UninstallDisplayIcon={app}\orbis.ico
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; MIT license + Fujifilm SDK notice, joined by scripts\package.ps1. The user
; must accept both, which passes the SDK obligations on (SDK agreement 2(b)).
LicenseFile={#Root}\target\installer\license.txt
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "cli"; Description: "Command-line tools (rig, cam) on PATH"; GroupDescription: "Extras:"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\orbis.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\rig.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\cam.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\docs\img\orbis.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
; Fujifilm SDK runtime (REDISTRIBUTABLE folder only), found next to orbis.exe at startup.
Source: "{#Root}\sdk\bin\*"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\installer\fujifilm-sdk-notice.txt"; DestDir: "{app}"; DestName: "FUJIFILM-SDK-NOTICE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Orbis"; Filename: "{app}\orbis.exe"; IconFilename: "{app}\orbis.ico"; Comment: "Automated photogrammetry scans"
Name: "{autodesktop}\Orbis"; Filename: "{app}\orbis.exe"; IconFilename: "{app}\orbis.ico"; Tasks: desktopicon

[Registry]
; Add {app} to the user's PATH for the cli task (removed again on uninstall).
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
    ValueData: "{olddata};{app}"; Tasks: cli; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\orbis.exe"; Description: "{cm:LaunchProgram,Orbis}"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Dir: string): Boolean;
var Path: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Result := True
  else
    Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var Path, Dir: string; P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then Exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then Exit;
  Dir := ';' + ExpandConstant('{app}');
  P := Pos(Uppercase(Dir), Uppercase(Path));
  if P > 0 then begin
    Delete(Path, P, Length(Dir));
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
  end;
end;
