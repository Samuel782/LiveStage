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
            TimecodeText(timecode.currentTimecode)

            // Visualizzazione del video con controlli visibili
            VideoPlayer(player: playerModel.player)
                .frame(height: 300)
                .padding()

            // Controlli di riproduzione
            HStack {
                Button("Play") {
                    playerModel.player.play()
                    
                    timecode.play()
                }
                .padding()

                Button("Pause") {
                    playerModel.player.pause()
                    timecode.pause()
                }
                .padding()
                Button("OpenFile") {
                    openFile()
                }.padding()
            }
            
            // CueList
            List(cueList){
                CueListRow(cue: $0)
            }
            
            // Pulsante per aprire la Clean View
            Button("Mostra Clean View") {
                createCleanWindow(with: playerModel)
            }
            .padding()
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
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            //LoadFile
            cueList.append(AnyCue(VideoCue(id: UUID(), title: url.absoluteString, player: AVPlayer(url: url))))
        
            playerModel.playVideo(url: url)
        }
    }

}
