//
//  CleanView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI
import AVKit


struct CleanView: View {
    @EnvironmentObject var playerModel: PlayerModel
    
    var body: some View {
        CustomVideoPlayer(player: playerModel.player)
            .edgesIgnoringSafeArea(.all)
        
    
    }
}
