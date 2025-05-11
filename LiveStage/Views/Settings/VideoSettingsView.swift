//
//  VideoSettingsView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 27/04/25.
//
import SwiftUI

extension NSScreen {
    var screenID: NSNumber? {
        deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber
    }
}

struct VideoSettingsView: View {
    
     @State private var screens: [NSScreen] = NSScreen.screens
     @State private var monitors: [FullScreenWindow] = []

     var body: some View {
         VStack(alignment: .leading) {
             Text("Setup outputs")
                 .font(.headline)

             Button(action: newMonitor) {
                 Label("New Output", systemImage: "display.2")
             }
             .padding(.vertical)

             List($monitors) { $monitor in
                 HStack {
                     Text(monitor.name)
                     Spacer()
                     Picker("", selection: $monitor.screen) {
                         // Show monitor only if isn't in use
                         ForEach(screens.filter { screen in
                             !monitors.contains(where: { $0.id != monitor.id && $0.screen == screen }) || monitor.screen == screen
                         }, id: \.screenID) { screen in
                             Text(screen.localizedName)
                                 .tag(screen as NSScreen?)
                         }
                     }
                     .pickerStyle(.menu)
                 }
             }
         }
         .padding()
     }

     func newMonitor() {
         monitors.append(FullScreenWindow(id: UUID(), name: "Output \(monitors.count + 1)", screen: nil))
     }

}
