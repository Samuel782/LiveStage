//
//  PlayerModel.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//



import SwiftUI
import AVKit

class PlayerModel: ObservableObject {
    @Published var player: AVPlayer
    
    init() {
        // Sostituisci con l'URL del tuo video
        guard let url = URL(string: "https://tuo-link-video.com/video.mp4") else {
            fatalError("URL video non valido")
        }
        self.player = AVPlayer(url: url)
    }
}
