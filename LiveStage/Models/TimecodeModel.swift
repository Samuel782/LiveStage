//
//  TimecodeModel.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI
import TimecodeKit

class TimecodeModel: ObservableObject {
    @Published var currentTimecode: Timecode = try! Timecode(.string("00:00:00:00"), at: .fps23_976)
    private var timer: Timer?
    private var elapsedTime: TimeInterval = 0 // Tempo trascorso in secondi

    // Funzione per avviare il play
    func play() {
        // Inizia il timer per aggiornare il timecode
        startTimecodeTimer()
    }

    // Funzione per fermare il play
    func pause() {
        // Ferma il timer
        timer?.invalidate()
    }
    
    func jumpTo(tc: Timecode){
        self.currentTimecode = tc
    }

    // Improve
    private func startTimecodeTimer() {
        // Interrompe un timer precedente se esiste
        timer?.invalidate()
        
        // Timer che aggiorna il timecode ogni 0.1s
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
            self.elapsedTime += 0.1 // Aumenta il tempo trascorso

            // Crea un nuovo timecode basato sul tempo trascorso
            self.currentTimecode = try! Timecode(.realTime(seconds: self.elapsedTime), at: .fps23_976)
        }
    }
}
