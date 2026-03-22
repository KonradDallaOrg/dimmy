using Microsoft.UI.Xaml;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class OnboardingWindow : Window
{
    public OnboardingViewModel ViewModel { get; } = new();

    public OnboardingWindow()
    {
        this.InitializeComponent();
        ((FrameworkElement)Content).DataContext = ViewModel;
        Title = "Dimmy";

        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Resize(new global::Windows.Graphics.SizeInt32(520, 440));

        if (appWindow?.Presenter is Microsoft.UI.Windowing.OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.IsMaximizable = false;
        }
    }

    private void NextStep_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.NextStep();

        // When reaching "Try It" (step 2), show the pill so user can test recording
        if (ViewModel.CurrentStep == 2)
        {
            App.Instance?.ShowPillAndHotkey();
        }
    }

    private void PrevStep_Click(object sender, RoutedEventArgs e) => ViewModel.PreviousStep();

    private void FinishOnboarding_Click(object sender, RoutedEventArgs e)
    {
        this.Close();
    }
}
