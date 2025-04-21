//
//  VideoCue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI
import AVKit

class VideoCue: Cue {

    public let player: AVPlayer

    init(player: AVPlayer, title: String, notes: String? = nil) {
        self.player = player
        super.init(title: title, notes: notes)
    }
    
    
}
