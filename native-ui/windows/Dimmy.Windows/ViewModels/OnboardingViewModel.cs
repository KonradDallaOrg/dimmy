using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

public partial class OnboardingViewModel : ObservableObject
{
    public int TotalSteps => 3;

    [ObservableProperty] private int _currentStep;
    [ObservableProperty] private string _shortcut = "Win+Alt";
    [ObservableProperty] private string _shortcutMode = "toggle";
    [ObservableProperty] private bool _isTrialSuccess;
    [ObservableProperty] private string _trialText = "";
    [ObservableProperty] private bool _isRecordingTrial;

    public bool CanGoBack => CurrentStep > 0;
    public bool IsStep0 => CurrentStep == 0;
    public bool IsStep1 => CurrentStep == 1;
    public bool IsStep2 => CurrentStep == 2;
    public bool Step1Reached => CurrentStep >= 1;
    public bool Step2Reached => CurrentStep >= 2;

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
        OnPropertyChanged(nameof(Step1Reached));
        OnPropertyChanged(nameof(Step2Reached));
    }
}
