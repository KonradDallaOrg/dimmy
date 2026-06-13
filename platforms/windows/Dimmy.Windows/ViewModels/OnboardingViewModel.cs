using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

public enum ModelChoice { None, Local, Cloud }

public partial class OnboardingViewModel : ObservableObject
{
    public int TotalSteps => 4;

    [ObservableProperty] private int _currentStep;
    [ObservableProperty] private string _shortcut = "Win+Alt";
    // Must match the Rust config default ("hold", core/src/lib.rs) AND the
    // wizard copy ("Hold to dictate, release to paste"): the trial step runs
    // with the Rust default, so persisting a different mode at Finish breaks
    // the taught gesture on the next launch.
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(TrialPromptVerb))]
    private string _shortcutMode = "hold";
    [ObservableProperty] private bool _isTrialSuccess;
    [ObservableProperty] private string _trialText = "";
    [ObservableProperty] private bool _isRecordingTrial;
    // True once a transcript has come back during the Try-it step. Drives the
    // inline "Transcribed" confirmation + the Continue button, instead of
    // auto-jumping to the success screen the instant text arrives (which hid
    // the result before the user could read it).
    [ObservableProperty] private bool _trialHasResult;

    // Live status shown in the trial box while there's no result yet, so the
    // user sees the hold working ("Listening...") and the post-release STT
    // round-trip ("Transcribing...") instead of a box that looks frozen for
    // the ~2 s it takes to come back.
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(TrialBoxPlaceholder))]
    private string _trialStatus = "";

    public string TrialBoxPlaceholder =>
        string.IsNullOrEmpty(TrialStatus) ? "Your transcription will appear here." : TrialStatus;

    // Mode-aware verb for the Try-it prompt: PTT is "Hold", toggle is "Press".
    // The old copy was hardcoded "Hold", which lied in toggle mode.
    public string TrialPromptVerb => ShortcutMode == "hold" ? "Hold" : "Press";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsLocalSelected))]
    [NotifyPropertyChangedFor(nameof(IsCloudSelected))]
    [NotifyPropertyChangedFor(nameof(CanAdvanceFromChoiceStep))]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private ModelChoice _choice = ModelChoice.None;

    [ObservableProperty] private double _downloadPercent;
    [ObservableProperty] private string _downloadStatusText = "Preparing...";
    [ObservableProperty] private string _downloadBytesText = "";

    /// Sentinel tag of the currently selected local model in the Local
    /// card ComboBox. For whisper sizes this is the whisper model
    /// filename (e.g. "ggml-base-q8_0.bin"). For Parakeet it is the
    /// magic value "parakeet:fp32". Persisted to config on Next.
    [ObservableProperty] private string _selectedLocalModelTag = "";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanAdvanceFromChoiceStep))]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isLocalReady;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanAdvanceFromChoiceStep))]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isLocalFailed;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanAdvanceFromChoiceStep))]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isLocalOffline;

    [ObservableProperty] private string _localErrorText = "";

    [ObservableProperty] private string _groqApiKey = "";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isValidatingKey;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanAdvanceFromChoiceStep))]
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isCloudReady;

    [ObservableProperty] private string _cloudErrorText = "";

    public bool CanGoBack => CurrentStep > 0;
    public bool IsStep0 => CurrentStep == 0;
    public bool IsStep1 => CurrentStep == 1;
    public bool IsStep2 => CurrentStep == 2;
    public bool IsStep3 => CurrentStep == 3;
    public bool Step1Reached => CurrentStep >= 1;
    public bool Step2Reached => CurrentStep >= 2;
    public bool Step3Reached => CurrentStep >= 3;

    public bool IsLocalSelected => Choice == ModelChoice.Local;
    public bool IsCloudSelected => Choice == ModelChoice.Cloud;

    /// Enables the choice-step button. Failed/offline count as clickable
    /// because the button then acts as Retry (NextStep_Click routes the
    /// click to a fresh download instead of advancing). Without this the
    /// label said "Try again" on a permanently disabled button.
    public bool CanAdvanceFromChoiceStep =>
        (Choice == ModelChoice.Local && (IsLocalReady || IsLocalFailed || IsLocalOffline)) ||
        (Choice == ModelChoice.Cloud && IsCloudReady);

    public bool IsLocalRetryable => IsLocalFailed || IsLocalOffline;

    public string ChoiceContinueLabel => Choice switch
    {
        ModelChoice.Local when IsLocalFailed => "Try again",
        ModelChoice.Local when IsLocalOffline => "Try again",
        ModelChoice.Local when !IsLocalReady => "Preparing model...",
        ModelChoice.Cloud when IsValidatingKey => "Verifying...",
        _ => "Continue",
    };

    public void NextStep()
    {
        if (CurrentStep < TotalSteps - 1)
        {
            CurrentStep++;
            NotifyStepProperties();
        }
    }

    public void PreviousStep()
    {
        if (CurrentStep > 0)
        {
            CurrentStep--;
            NotifyStepProperties();
        }
    }

    private void NotifyStepProperties()
    {
        OnPropertyChanged(nameof(CanGoBack));
        OnPropertyChanged(nameof(IsStep0));
        OnPropertyChanged(nameof(IsStep1));
        OnPropertyChanged(nameof(IsStep2));
        OnPropertyChanged(nameof(IsStep3));
        OnPropertyChanged(nameof(Step1Reached));
        OnPropertyChanged(nameof(Step2Reached));
        OnPropertyChanged(nameof(Step3Reached));
    }
}
