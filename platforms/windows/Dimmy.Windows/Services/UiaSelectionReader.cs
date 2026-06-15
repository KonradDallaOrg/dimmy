using System;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Services;

/// <summary>
/// Read the currently-selected text from the focused control via UI
/// Automation's <c>TextPattern.GetSelection</c> — NO clipboard, NO
/// synthetic keystrokes. This is the reliable path for UIA-aware apps
/// (Outlook, Word, WordPad, classic edit/RichEdit controls, most native
/// Win32 + WinUI apps). It is read at command-record PRESS time, while the
/// user's selection + focus are pristine, which also makes it safe in
/// hold (PTT) mode: a synthetic Ctrl+C would inject a Ctrl-up that the
/// global hook reads as "combo released" and stops recording early. UIA
/// touches neither the keyboard nor the clipboard, so there's no conflict.
///
/// Apps that do NOT implement the UIA TextPattern (Electron: VS Code,
/// Slack, Notion, some browsers) return null here; the caller falls back
/// to the synthetic-Ctrl+C grab in <see cref="SelectionCaptureService"/>.
///
/// The COM interfaces are declared by hand with EXACT vtable ordering from
/// uiautomationclient.h — only the methods we call carry real signatures;
/// every preceding method is a one-slot placeholder to preserve the
/// offset. A miscount would AccessViolation, so this is covered by a
/// manual WordPad/Notepad smoke test before shipping. Everything is
/// wrapped so any failure degrades to null (→ Ctrl+C fallback), never a
/// crash in the user's foreground app.
/// </summary>
public static class UiaSelectionReader
{
    private const int UIA_TextPatternId = 10014;

    private static readonly Guid CLSID_CUIAutomation =
        new("ff48dba4-60ef-4201-aa87-54103eef594e");

    /// <summary>Return the focused element's selected text, or null when
    /// there is no selection, the app doesn't support UIA text, or anything
    /// goes wrong. Never throws. Call from a background thread (MTA) — UIA
    /// cross-process calls can block briefly.</summary>
    public static string? TryGetFocusedSelection()
    {
        object? automationObj = null;
        try
        {
            var t = Type.GetTypeFromCLSID(CLSID_CUIAutomation);
            if (t == null) return null;
            automationObj = Activator.CreateInstance(t);
            if (automationObj is not IUIAutomation automation) return null;

            if (automation.GetFocusedElement(out var element) != 0 || element == null)
                return null;

            try
            {
                if (element.GetCurrentPattern(UIA_TextPatternId, out var patternPtr) != 0
                    || patternPtr == IntPtr.Zero)
                    return null;

                object? patternObj = null;
                try
                {
                    patternObj = Marshal.GetObjectForIUnknown(patternPtr);
                    if (patternObj is not IUIAutomationTextPattern textPattern) return null;

                    if (textPattern.GetSelection(out var ranges) != 0 || ranges == null)
                        return null;

                    try
                    {
                        if (ranges.get_Length(out int len) != 0 || len <= 0) return null;
                        if (ranges.GetElement(0, out var range) != 0 || range == null) return null;
                        try
                        {
                            // -1 = the whole range; UIA may cap very long text,
                            // which is fine for a command-mode selection.
                            if (range.GetText(-1, out var text) != 0) return null;
                            text = text?.Trim();
                            return string.IsNullOrEmpty(text) ? null : text;
                        }
                        finally { Marshal.ReleaseComObject(range); }
                    }
                    finally { Marshal.ReleaseComObject(ranges); }
                }
                finally
                {
                    if (patternObj != null) Marshal.ReleaseComObject(patternObj);
                    Marshal.Release(patternPtr);
                }
            }
            finally { Marshal.ReleaseComObject(element); }
        }
        catch
        {
            return null;
        }
        finally
        {
            if (automationObj != null)
            {
                try { Marshal.ReleaseComObject(automationObj); } catch { }
            }
        }
    }

    // ── COM interop (exact vtable order from uiautomationclient.h) ────────
    // Only the called methods have real signatures; `_slotN` entries are
    // one-slot placeholders preserving the offset to the method we use.

    [ComImport, Guid("30cbe57d-d9d0-452a-ab13-7ac5ac4825ee"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IUIAutomation
    {
        [PreserveSig] int _CompareElements();          // 1
        [PreserveSig] int _CompareRuntimeIds();        // 2
        [PreserveSig] int _GetRootElement();           // 3
        [PreserveSig] int _ElementFromHandle();        // 4
        [PreserveSig] int _ElementFromPoint();         // 5
        [PreserveSig] int GetFocusedElement(           // 6
            [MarshalAs(UnmanagedType.Interface)] out IUIAutomationElement? element);
    }

    [ComImport, Guid("d22108aa-8ac5-49a5-837b-37bbb3d7591e"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IUIAutomationElement
    {
        [PreserveSig] int _SetFocus();                     // 1
        [PreserveSig] int _GetRuntimeId();                 // 2
        [PreserveSig] int _FindFirst();                    // 3
        [PreserveSig] int _FindAll();                      // 4
        [PreserveSig] int _FindFirstBuildCache();          // 5
        [PreserveSig] int _FindAllBuildCache();            // 6
        [PreserveSig] int _BuildUpdatedCache();            // 7
        [PreserveSig] int _GetCurrentPropertyValue();      // 8
        [PreserveSig] int _GetCurrentPropertyValueEx();    // 9
        [PreserveSig] int _GetCachedPropertyValue();       // 10
        [PreserveSig] int _GetCachedPropertyValueEx();     // 11
        [PreserveSig] int _GetCurrentPatternAs();          // 12
        [PreserveSig] int _GetCachedPatternAs();           // 13
        [PreserveSig] int GetCurrentPattern(               // 14
            int patternId, out IntPtr patternObject);
    }

    [ComImport, Guid("32eba289-3583-42c9-9c59-3b6d9a1e9b6a"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IUIAutomationTextPattern
    {
        [PreserveSig] int _RangeFromPoint();   // 1
        [PreserveSig] int _RangeFromChild();   // 2
        [PreserveSig] int GetSelection(        // 3
            [MarshalAs(UnmanagedType.Interface)] out IUIAutomationTextRangeArray? ranges);
    }

    [ComImport, Guid("ce4ae76a-e717-4c98-81ea-47371d028eb6"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IUIAutomationTextRangeArray
    {
        [PreserveSig] int get_Length(out int length);   // 1
        [PreserveSig] int GetElement(                   // 2
            int index, [MarshalAs(UnmanagedType.Interface)] out IUIAutomationTextRange? element);
    }

    [ComImport, Guid("a543cc6a-f4ae-494b-8239-c814481187a8"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IUIAutomationTextRange
    {
        [PreserveSig] int _Clone();                 // 1
        [PreserveSig] int _Compare();               // 2
        [PreserveSig] int _CompareEndpoints();      // 3
        [PreserveSig] int _ExpandToEnclosingUnit(); // 4
        [PreserveSig] int _FindAttribute();         // 5
        [PreserveSig] int _FindText();              // 6
        [PreserveSig] int _GetAttributeValue();     // 7
        [PreserveSig] int _GetBoundingRectangles(); // 8
        [PreserveSig] int _GetEnclosingElement();   // 9
        [PreserveSig] int GetText(                  // 10
            int maxLength, [MarshalAs(UnmanagedType.BStr)] out string text);
    }
}
