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
        self.player = AVPlayer()
    }
    
    func playVideo(url: URL){
        player = AVPlayer(url: url)
    }
    
    func playVideo(cue: VideoCue){
        player = cue.player
    }
    
}
