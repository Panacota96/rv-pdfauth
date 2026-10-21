; Inno Setup script for RV PDFAuth (built by .github/workflows/release.yml).
; Per-user install: no administrator rights needed on the examiner's PC.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\.."
#endif

[Setup]
AppId={{B2454915-B966-44CD-9C5B-5413F38185AE}
AppName=RV PDFAuth
AppVersion={#AppVersion}
AppPublisher=Rakvor
AppPublisherURL=https://github.com/Panacota96/rv-pdfauth
DefaultDirName={localappdata}\Programs\Rakvor\RV PDFAuth
DefaultGroupName=Rakvor
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile={#SourceDir}\LICENSE
OutputBaseFilename=rv-pdfauth-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile={#SourceDir}\assets\brand\rv.ico
ChangesEnvironment=yes
UninstallDisplayIcon={app}\rv-pdfauth-gui.exe

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "addtopath"; Description: "Add the rv-pdfauth command to PATH"; Flags: checkedonce
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\target\release\rv-pdfauth.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\target\release\rv-pdfauth-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
; PDFium (BSD-3, loaded at run time for the visual diff) and its licences: see NOTICE.
Source: "{#SourceDir}\target\release\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\third_party\pdfium\win-x64\licenses\*"; DestDir: "{app}\licenses\pdfium"
Source: "{#SourceDir}\third_party\pdfium\win-x64\LICENSE-pdfium-binaries.txt"; DestDir: "{app}\licenses\pdfium"
Source: "{#SourceDir}\README.md"; DestDir: "{app}"
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"
Source: "{#SourceDir}\NOTICE"; DestDir: "{app}"
Source: "{#SourceDir}\CHANGELOG.md"; DestDir: "{app}"
Source: "{#SourceDir}\docs\*"; DestDir: "{app}\docs"; Flags: recursesubdirs
Source: "{#SourceDir}\examples\sample\evidence\*"; DestDir: "{app}\examples\sample\evidence"

[Icons]
Name: "{group}\RV PDFAuth"; Filename: "{app}\rv-pdfauth-gui.exe"
Name: "{group}\RV PDFAuth documentation"; Filename: "{app}\docs"
Name: "{autodesktop}\RV PDFAuth"; Filename: "{app}\rv-pdfauth-gui.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\rv-pdfauth-gui.exe"; Description: "Start RV PDFAuth"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

{ Uninstall: remove every occurrence of the install folder from the user PATH (HKCU\Environment) that the
  addtopath task added; other entries are kept in their order. ChangesEnvironment=yes makes the uninstaller
  broadcast the change, so new terminals no longer see the folder. }
procedure RemoveFromPath(Dir: string);
var
  Paths, Padded, Upper, Needle: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    exit;
  Padded := ';' + Paths + ';';
  Needle := ';' + Uppercase(Dir) + ';';
  Upper := Uppercase(Padded);
  P := Pos(Needle, Upper);
  if P = 0 then
    exit;
  while P > 0 do
  begin
    { Delete ";<Dir>" and keep the following ';' as the separator. }
    Delete(Padded, P, Length(Dir) + 1);
    Upper := Uppercase(Padded);
    P := Pos(Needle, Upper);
  end;
  { Strip the two padding separators (Padded is ';' alone when Dir was the only entry). }
  if Length(Padded) <= 2 then
    Paths := ''
  else
    Paths := Copy(Padded, 2, Length(Padded) - 2);
  if Paths = '' then
    RegDeleteValue(HKEY_CURRENT_USER, 'Environment', 'Path')
  else
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
