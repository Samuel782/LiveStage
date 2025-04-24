import AVKit
import SwiftUI
import AVFoundation
//
//  VideoCue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//


class VideoCue: Cue {
    // Cue Protocol
    var id: UUID
    var title: String
    var notes: String?
    var cueDuration: Double

    public let player: AVPlayer

    init(id: UUID, title: String, notes: String? = nil, player: AVPlayer) {
        self.id = id
        self.title = title
        self.notes = notes
        self.player = player
        self.cueDuration = 0.0

        guard let asset = player.currentItem?.asset else {
            return
        }
        
        loadDuration(from: asset)
        
    }
    
    private func loadDuration(from asset: AVAsset) {
        Task {
            do {
                let duration = try await asset.load(.duration)
                if duration.isValid {
                    let durationInSeconds = CMTimeGetSeconds(duration)
                    if durationInSeconds.isFinite {
                        self.cueDuration = durationInSeconds
                    } else {
                        print("Durata non valida.")
                    }
                } else {
                    print("CMTime non valido.")
                }
            } catch {
                print("Errore nel caricamento della durata: \(error.localizedDescription)")
            }
        }
    }


    

    func cueView() -> AnyView {
        AnyView(
            Text("Preview del cue: \(title)")
                .padding()
        )
    }

}
