// CustomAction.cpp : Defines the entry point for the custom action.
#include "pch.h"
#include <stdlib.h>
#include <strutil.h>
#include <shellapi.h>
#include <tlhelp32.h>
#include <winternl.h>
#include <netfw.h>
#include <shlwapi.h>

#pragma comment(lib, "Shlwapi.lib")

UINT __stdcall CustomActionHello(
    __in MSIHANDLE hInstall)
{
    HRESULT hr = S_OK;
    DWORD er = ERROR_SUCCESS;

    hr = WcaInitialize(hInstall, "CustomActionHello");
    ExitOnFailure(hr, "Failed to initialize");

    WcaLog(LOGMSG_STANDARD, "Initialized.");

    // TODO: Add your custom action code here.
    WcaLog(LOGMSG_STANDARD, "================= Example CustomAction Hello");

LExit:
    er = SUCCEEDED(hr) ? ERROR_SUCCESS : ERROR_INSTALL_FAILURE;
    return WcaFinalize(er);
}

UINT __stdcall RemoveInstallFolder(
    __in MSIHANDLE hInstall)
{
    HRESULT hr = S_OK;
    DWORD er = ERROR_SUCCESS;

    int nResult = 0;
    LPWSTR installFolder = NULL;
    LPWSTR pwz = NULL;
    LPWSTR pwzData = NULL;

    hr = WcaInitialize(hInstall, "RemoveInstallFolder");
    ExitOnFailure(hr, "Failed to initialize");

    hr = WcaGetProperty(L"CustomActionData", &pwzData);
    ExitOnFailure(hr, "failed to get CustomActionData");

    pwz = pwzData;
    hr = WcaReadStringFromCaData(&pwz, &installFolder);
    ExitOnFailure(hr, "failed to read database key from custom action data: %ls", pwz);

    SHFILEOPSTRUCTW fileOp;
    ZeroMemory(&fileOp, sizeof(SHFILEOPSTRUCT));

    fileOp.wFunc = FO_DELETE;
    fileOp.pFrom = installFolder;
    fileOp.fFlags = FOF_NOCONFIRMATION | FOF_SILENT;

    nResult = SHFileOperation(&fileOp);
    if (nResult == 0)
    {
        WcaLog(LOGMSG_STANDARD, "The directory \"%ls\" has been deleted.", installFolder);
    }
    else
    {
        WcaLog(LOGMSG_STANDARD, "The directory \"%ls\" has not been deleted, error code: 0X%02X. Please refer to https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shfileoperationa for the error codes.", installFolder, nResult);
    }

LExit:
    ReleaseStr(installFolder);

    er = SUCCEEDED(hr) ? ERROR_SUCCESS : ERROR_INSTALL_FAILURE;
    return WcaFinalize(er);
}

// https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryinformationprocess
// **NtQueryInformationProcess** may be altered or unavailable in future versions of Windows.
// Applications should use the alternate functions listed in this topic.
// But I do not find the alternate functions.
// https://github.com/heim-rs/heim/issues/105#issuecomment-683647573
typedef NTSTATUS(NTAPI *pfnNtQueryInformationProcess)(HANDLE, PROCESSINFOCLASS, PVOID, ULONG, PULONG);
bool TerminateProcessIfNotContainsParam(pfnNtQueryInformationProcess NtQueryInformationProcess, HANDLE process, LPCWSTR excludeParam)
{
    bool processClosed = false;
    PROCESS_BASIC_INFORMATION processInfo;
    NTSTATUS status = NtQueryInformationProcess(process, ProcessBasicInformation, &processInfo, sizeof(processInfo), NULL);
    if (status == 0 && processInfo.PebBaseAddress != NULL)
    {
        PEB peb;
        SIZE_T dwBytesRead;
        if (ReadProcessMemory(process, processInfo.PebBaseAddress, &peb, sizeof(peb), &dwBytesRead))
        {
            RTL_USER_PROCESS_PARAMETERS pebUpp;
            if (ReadProcessMemory(process,
                                  peb.ProcessParameters,
                                  &pebUpp,
                                  sizeof(RTL_USER_PROCESS_PARAMETERS),
                                  &dwBytesRead))
            {
                if (pebUpp.CommandLine.Length > 0)
                {
                    WCHAR *commandLine = (WCHAR *)malloc(pebUpp.CommandLine.Length);
                    if (commandLine != NULL)
                    {
                        if (ReadProcessMemory(process, pebUpp.CommandLine.Buffer,
                                              commandLine, pebUpp.CommandLine.Length, &dwBytesRead))
                        {
                            if (wcsstr(commandLine, excludeParam) == NULL)
                            {
                                WcaLog(LOGMSG_STANDARD, "Terminate process : %ls", commandLine);
                                TerminateProcess(process, 0);
                                processClosed = true;
                            }
                        }
                        free(commandLine);
                    }
                }
            }
        }
    }
    return processClosed;
}

// Terminate processes that do not have parameter [excludeParam]
// Note. This function relies on "NtQueryInformationProcess",
//       which may not be found.
//       Then all processes of [processName] will be terminated.
bool TerminateProcessesByNameW(LPCWSTR processName, LPCWSTR excludeParam)
{
    HMODULE hntdll = GetModuleHandleW(L"ntdll.dll");
    if (hntdll == NULL)
    {
        WcaLog(LOGMSG_STANDARD, "Failed to load ntdll.");
    }

    pfnNtQueryInformationProcess NtQueryInformationProcess = NULL;
    if (hntdll != NULL)
    {
        NtQueryInformationProcess = (pfnNtQueryInformationProcess)GetProcAddress(
            hntdll, "NtQueryInformationProcess");
    }
    if (NtQueryInformationProcess == NULL)
    {
        WcaLog(LOGMSG_STANDARD, "Failed to get address of NtQueryInformationProcess.");
    }

    bool processClosed = false;
    // Create a snapshot of the current system processes
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot != INVALID_HANDLE_VALUE)
    {
        PROCESSENTRY32W processEntry;
        processEntry.dwSize = sizeof(PROCESSENTRY32W);
        if (Process32FirstW(snapshot, &processEntry))
        {
            do
            {
                if (lstrcmpW(processName, processEntry.szExeFile) == 0)
                {
                    HANDLE process = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, processEntry.th32ProcessID);
                    if (process != NULL)
                    {
                        if (NtQueryInformationProcess == NULL)
                        {
                            WcaLog(LOGMSG_STANDARD, "Terminate process : %ls, while NtQueryInformationProcess is NULL", processName);
                            TerminateProcess(process, 0);
                            processClosed = true;
                        }
                        else
                        {
                            processClosed = TerminateProcessIfNotContainsParam(
                                NtQueryInformationProcess,
                                process,
                                excludeParam);
                        }
                        CloseHandle(process);
                    }
                }
            } while (Process32Next(snapshot, &processEntry));
        }
        CloseHandle(snapshot);
    }
    if (hntdll != NULL)
    {
        CloseHandle(hntdll);
    }
    return processClosed;
}

UINT __stdcall TerminateProcesses(
    __in MSIHANDLE hInstall)
{
    HRESULT hr = S_OK;
    DWORD er = ERROR_SUCCESS;

    int nResult = 0;
    wchar_t szProcess[256] = {0};
    DWORD cchProcess = sizeof(szProcess) / sizeof(szProcess[0]);

    hr = WcaInitialize(hInstall, "TerminateProcesses");
    ExitOnFailure(hr, "Failed to initialize");

    MsiGetPropertyW(hInstall, L"TerminateProcesses", szProcess, &cchProcess);

    WcaLog(LOGMSG_STANDARD, "Try terminate processes : %ls", szProcess);
    TerminateProcessesByNameW(szProcess, L"--install");

LExit:
    er = SUCCEEDED(hr) ? ERROR_SUCCESS : ERROR_INSTALL_FAILURE;
    return WcaFinalize(er);
}

// No use for now, it can be refer as an example of ShellExecuteW.
void AddFirewallRuleCmdline(LPWSTR exeName, LPWSTR exeFile, LPCWSTR dir)
{
    HRESULT hr = S_OK;
    HINSTANCE hi = 0;
    WCHAR cmdline[1024] = { 0, };
    WCHAR rulename[500] = { 0, };

    StringCchPrintfW(rulename, sizeof(rulename) / sizeof(rulename[0]), L"%ls Service", exeName);
    if (hr < 0) {
        WcaLog(LOGMSG_STANDARD, "Failed to make rulename: %ls", exeName);
        return;
    }

    StringCchPrintfW(cmdline, sizeof(cmdline) / sizeof(cmdline[0]), L"advfirewall firewall add rule name=\"%ls\" dir=%ls action=allow program=\"%ls\" enable=yes", rulename, dir, exeFile);
    if (hr < 0) {
        WcaLog(LOGMSG_STANDARD, "Failed to make cmdline: %ls", exeName);
        return;
    }

    hi = ShellExecuteW(NULL, L"open", L"netsh", cmdline, NULL, SW_HIDE);
    // https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew
    if ((int)hi <= 32) {
        WcaLog(LOGMSG_STANDARD, "Failed to change firewall rule : %d, last error: %d", (int)hi, GetLastError());
    }
    else {
        WcaLog(LOGMSG_STANDARD, "Firewall rule \"%ls\" (%ls) is added", rulename, dir);
    }
}

// No use for now, it can be refer as an example of ShellExecuteW.
void RemoveFirewallRuleCmdline(LPWSTR exeName)
{
    HRESULT hr = S_OK;
    HINSTANCE hi = 0;
    WCHAR cmdline[1024] = { 0, };
    WCHAR rulename[500] = { 0, };

    StringCchPrintfW(rulename, sizeof(rulename) / sizeof(rulename[0]), L"%ls Service", exeName);
    if (hr < 0) {
        WcaLog(LOGMSG_STANDARD, "Failed to make rulename: %ls", exeName);
        return;
    }

    StringCchPrintfW(cmdline, sizeof(cmdline) / sizeof(cmdline[0]), L"advfirewall firewall delete rule name=\"%ls\"", rulename);
    if (hr < 0) {
        WcaLog(LOGMSG_STANDARD, "Failed to make cmdline: %ls", exeName);
        return;
    }

    hi = ShellExecuteW(NULL, L"open", L"netsh", cmdline, NULL, SW_HIDE);
    // https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew
    if ((int)hi <= 32) {
        WcaLog(LOGMSG_STANDARD, "Failed to change firewall rule \"%ls\" : %d, last error: %d", rulename, (int)hi, GetLastError());
    }
    else {
        WcaLog(LOGMSG_STANDARD, "Firewall rule \"%ls\" is removed", rulename);
    }
}

bool AddFirewallRule(bool add, LPWSTR exeName, LPWSTR exeFile);
UINT __stdcall AddFirewallRules(
    __in MSIHANDLE hInstall)
{
    HRESULT hr = S_OK;
    DWORD er = ERROR_SUCCESS;

    int nResult = 0;
    LPWSTR exeFile = NULL;
    LPWSTR exeName = NULL;
    WCHAR exeNameNoExt[500] = { 0, };
    LPWSTR pwz = NULL;
    LPWSTR pwzData = NULL;
    size_t szNameLen = 0;

    hr = WcaInitialize(hInstall, "AddFirewallExceptions");
    ExitOnFailure(hr, "Failed to initialize");

    hr = WcaGetProperty(L"CustomActionData", &pwzData);
    ExitOnFailure(hr, "failed to get CustomActionData");

    pwz = pwzData;
    hr = WcaReadStringFromCaData(&pwz, &exeFile);
    ExitOnFailure(hr, "failed to read database key from custom action data: %ls", pwz);
    WcaLog(LOGMSG_STANDARD, "Try add firewall exceptions for file : %ls", exeFile);

    exeName = PathFindFileNameW(exeFile + 1);
    hr = StringCchPrintfW(exeNameNoExt, 500, exeName);
    ExitOnFailure(hr, "Failed to copy exe name: %ls", exeName);
    szNameLen = wcslen(exeNameNoExt);
    if (szNameLen >= 4 && wcscmp(exeNameNoExt + szNameLen - 4, L".exe") == 0) {
        exeNameNoExt[szNameLen - 4] = L'\0';
    }

    //if (exeFile[0] == L'1') {
    //    AddFirewallRuleCmdline(exeNameNoExt, exeFile, L"in");
    //    AddFirewallRuleCmdline(exeNameNoExt, exeFile, L"out");
    //}
    //else {
    //    RemoveFirewallRuleCmdline(exeNameNoExt);
    //}

    AddFirewallRule(exeFile[0] == L'1', exeNameNoExt, exeFile + 1);

LExit:
    if (pwzData) {
        ReleaseStr(pwzData);
    }

    er = SUCCEEDED(hr) ? ERROR_SUCCESS : ERROR_INSTALL_FAILURE;
    return WcaFinalize(er);
}
