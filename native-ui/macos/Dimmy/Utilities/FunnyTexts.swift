import Foundation

enum FunnyTexts {
    static let texts: [String] = [
        "Stavo per dire qualcosa di brillante, ma Dimmy ha capito questo",
        "Il mio gatto ha dettato questo messaggio",
        "Dimmy funziona, il problema sei tu",
        "Questo testo è stato generato da un criceto su una ruota",
        "Ho detto cose importantissime ma Dimmy ha preferito questo",
        "Se stai leggendo questo, la demo funziona",
        "Dimmy ha trascritto i tuoi pensieri più profondi... ed erano questi",
        "Il microfono funziona, il cervello è opzionale",
        "Messaggio dettato con successo. Contenuto: irrilevante.",
        "La mia eloquenza è stata ridotta a questa singola riga",
        "Tecnicamente ho detto qualcosa di meglio, ma va bene così",
        "Dimmy: 1, La tua produttività: 0",
        "Questo messaggio si autodistruggerà in... no, scherzo, resta qui",
        "Ho provato a dettare il senso della vita, ecco il risultato",
    ]

    static var random: String {
        texts.randomElement() ?? texts[0]
    }
}
