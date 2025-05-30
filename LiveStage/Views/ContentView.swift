//
//  ContentView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

import AVKit
import AppKit
import SwiftUI
import TimecodeKit
import TimecodeKitUI

struct ContentView: View {
    @StateObject var playerModel = PlayerModel()
    @StateObject var timecode = TimecodeModel()

    @State var cueList: [AnyCue] = []

    var body: some View {
        VStack {

            HStack {
                // CueList
                CueListView(cueList: $cueList)

                VStack {
                    // Visualizzazione del video con controlli visibili
                    VideoPlayer(player: playerModel.player)
                        .frame(height: 300)
                        .padding()

                    TimecodeText(timecode.currentTimecode)

                    // Controlli di riproduzione
                    HStack {
                        
                        Button("Play Selected") {
                            if let selectedCue = cueList.first(where: {
                                $0.isSelected
                            }) {
                                playerModel.player.pause()
                                let videoCue = selectedCue.cast(VideoCue.self)
                                playerModel.player = videoCue!.player
                                playerModel.player.play()
                            }
                        }.padding()
                        
                        Button("Pause") {
                            playerModel.player.pause()
                            timecode.pause()
                        }
                        .padding()
                        Button("OpenFile") {
                            openFile()
                        }.padding()

                        // Pulsante per aprire una finestra Clean View -  Temporaneo
                        Button("Mostra Clean View") {
                            createCleanWindow(with: playerModel)
                        }
                        .padding()
                    }
                }
            }

        }
        .environmentObject(playerModel)

    }

    // use NSScreen for clean View
    func createCleanWindow(with playerModel: PlayerModel) {
        // Configura la Clean View e inseriscila in un NSHostingController
        let cleanView = CleanView().environmentObject(playerModel)
        let hostingController = NSHostingController(rootView: cleanView)

        // Crea una nuova finestra con barra del titolo (Rimuovi)
        // Posizione e dimensioni iniziali
        // Stile della finestra: con titolo e ridimensionabile
        // Tipo di buffer usato per il rendering (standard)
        // Crea subito la finestra (non differisce la creazione)
        let window = NSWindow(
            contentRect: NSRect(x: 100, y: 100, width: 800, height: 600),
            styleMask: [.titled, .resizable],
            backing: .buffered,
            defer: false
        )
        // Imposta una dimensione minima per la finestra
        window.minSize = NSSize(width: 800, height: 600)

        // Assegna l'NSHostingController come contenuto della finestra
        window.contentViewController = hostingController

        // Imposta il titolo della finestra
        window.title = "Clean Player"

        // Mostra la finestra e la rende attiva
        window.makeKeyAndOrderFront(nil)
    }

    func openFile() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.movie]
        panel.allowsMultipleSelection = true
        if panel.runModal() == .OK {
            for url in panel.urls {
                cueList.append(
                    AnyCue(
                        VideoCue(
                            id: UUID(),
                            title: url.lastPathComponent,
                            player: AVPlayer(url: url)
                        )
                    )
                )
            }
            //playerModel.playVideo(url: url)
        }
        
    }

}
