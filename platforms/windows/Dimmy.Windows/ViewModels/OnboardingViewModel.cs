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
    [ObservableProperty] private string _shortcutMode = "hold";
    [ObservableProperty] private bool _isTrialSuccess;
    [ObservableProperty] private string _trialText = "";
    [ObservableProperty] private bool _isRecordingTrial;

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
    [NotifyPropertyChangedFor(nameof(ChoiceContinueLabel))]
    private bool _isLocalFailed;

    [ObservableProperty]
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

    public bool CanAdvanceFromChoiceStep =>
        (Choice == ModelChoice.Local && IsLocalReady) ||
        (Choice == ModelChoice.Cloud && IsCloudReady);

    public string ChoiceContinueLabel => Choice switch
    {
        ModelChoice.Local when IsLocalFailed => "Try again",
        ModelChoice.Local when IsLocalOffline => "Offline",
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
