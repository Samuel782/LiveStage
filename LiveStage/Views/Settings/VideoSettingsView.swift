//
//  VideoSettingsView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 27/04/25.
//
import SwiftUI

struct VideoSettingsView: View {

    var body: some View {
        Text("--- Video Setting ---")
        List(NSScreen.screens, id: \.self) { screen in
            VStack(alignment: .leading) {
                Text("Screen: \(screen.localizedName)")
                Text("Frame: \(screen.frame)")
                    .font(.footnote)
                    .foregroundColor(.gray)
            }
        }
    }

}
