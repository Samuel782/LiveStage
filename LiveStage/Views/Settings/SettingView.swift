//
//  SettingView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 27/04/25.
//
import SwiftUI

struct SettingView: View{
    var body: some View{
        NavigationSplitView(columnVisibility: .constant(.doubleColumn)) {
                   List {
                       NavigationLink(destination: VideoSettingsView()) {
                           Label("Video Setting", systemImage: "film")
                       }
                       NavigationLink(destination: FooView()) {
                           Label("Audio Setting", systemImage: "speaker")
                       }
                       NavigationLink(destination: FooView()) {
                           Label("MIDI Setting", systemImage: "music.note")
                       }

                   }
                   .padding(.top)
                   .frame(width: 215)
                   .toolbar(removing: .sidebarToggle)
               } detail: {
                   FooView()
               }
        .frame(minWidth: 715, maxWidth: 715, minHeight: 470, maxHeight: .infinity)

    }
}
struct FooView: View {
    var body: some View {
        Text("Foo View")
    }
}

#Preview {
    SettingView()
}
