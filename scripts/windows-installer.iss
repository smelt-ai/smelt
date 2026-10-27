; Windows 安装程序（Inno Setup 7，自带简体中文界面）。由 release.yml 的 windows 任务编：
;
;   ISCC /DAppVersion=<版本> /DAppVersionNumeric=<x.y.z> /DStageDir=<暂存目录> /DOutputDir=<输出目录> scripts\windows-installer.iss
;
; 暂存目录里是 smelt.exe、smelt.com、smeltd.exe、smelt-notify.exe、smelt-pi.exe 和 Pi 要的文件，
; 原样装进安装目录。按用户安装，不要管理员权限，默认装到 %LOCALAPPDATA%\Programs\Smelt。
; 数据目录 %USERPROFILE%\.smelt 不归安装程序管，卸载也不删。

#ifndef AppVersion
  #error 缺 /DAppVersion
#endif
#ifndef AppVersionNumeric
  #error 缺 /DAppVersionNumeric
#endif
#ifndef StageDir
  #error 缺 /DStageDir
#endif
#ifndef OutputDir
  #error 缺 /DOutputDir
#endif

[Setup]
; AppId 认的是「同一个应用」：升级覆盖装、卸载都靠它，定下就不能改。
AppId={{3FC5642C-9420-463A-BD6B-DADA7CB215E9}
AppName=Smelt
AppVersion={#AppVersion}
AppPublisher=smelt-ai
AppPublisherURL=https://github.com/smelt-ai/smelt
AppSupportURL=https://github.com/smelt-ai/smelt/issues
AppUpdatesURL=https://github.com/smelt-ai/smelt/releases
VersionInfoVersion={#AppVersionNumeric}
PrivilegesRequired=lowest
; 不要管理员权限时 {autopf} 就是 {userpf}，即 %LOCALAPPDATA%\Programs。
DefaultDirName={autopf}\Smelt
DisableProgramGroupPage=yes
SetupArchitecture=x64
; Windows 10 1803：AF_UNIX 从这一版起才有（docs/windows-support.md）。
MinVersion=10.0.17134
; 桌面运行时持有这个命名互斥量（smelt_sys::mark_desktop_running）。安装和卸载开头都查它，
; Smelt 还开着就提示先关掉，免得覆盖或删除正在用的 smelt.exe。
AppMutex=SmeltDesktop
ChangesEnvironment=yes
UninstallDisplayIcon={app}\smelt.exe
SetupIconFile=..\assets\smelt.ico
WizardStyle=modern
OutputDir={#OutputDir}
OutputBaseFilename=smelt-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "addtopath"; Description: "把 smelt 命令加入 PATH（在 cmd、PowerShell 里直接敲 smelt）"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; 这两个目录随 Pi 整份发布，旧版本多出来的文件要清掉，不能和新文件混在一起。
Type: filesandordirs; Name: "{app}\skills"
Type: filesandordirs; Name: "{app}\native"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Smelt"; Filename: "{app}\smelt.exe"
Name: "{autodesktop}\Smelt"; Filename: "{app}\smelt.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\smelt.exe"; Description: "{cm:LaunchProgram,Smelt}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 本机运行时跑的是复制到 %USERPROFILE%\.smelt\bin 的 smeltd-<指纹>.exe，不在安装目录里，卸载
; 删不到它，得先停掉，不然它和它起的会话一直留在后台。会话进程在它的 Job Object 里，一起退出。
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM smeltd-*"; Flags: runhidden; RunOnceId: "StopSmeltd"

[Code]
const
  EnvironmentKey = 'Environment';

{ 用户 PATH 里有没有这一项。按 ; 切开整段比，不区分大小写。 }
function PathHasEntry(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure AddToPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', Paths) then
    Paths := '';
  if PathHasEntry(Paths, Dir) then
    exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then
    Paths := Paths + ';';
  RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  At: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', Paths) then
    exit;
  Paths := ';' + Paths + ';';
  At := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
  if At = 0 then
    exit;
  { 删掉「;Dir」，再去掉首尾补上的 ;。 }
  Delete(Paths, At, Length(Dir) + 1);
  RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', Copy(Paths, 2, Length(Paths) - 2));
end;

{ PATH 跟着这次的勾选走：重装时取消勾选，就把上次加的那项拿掉。 }
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('addtopath') then
      AddToPath(ExpandConstant('{app}'))
    else
      RemoveFromPath(ExpandConstant('{app}'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
