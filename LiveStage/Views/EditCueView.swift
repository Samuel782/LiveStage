//
//  EditCueView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//
import SwiftUI

struct EditCueView: View {
    @Binding var cue: AnyCue

    var body: some View {
        VStack {
            // Modifica del titolo
            TextField("Title:", text: $cue.title)
                .textFieldStyle(RoundedBorderTextFieldStyle())
                .padding()

            // Modifica delle note
            TextField("Notes:", text: Binding(
                get: { cue.notes ?? "" },
                set: { cue.notes = $0 }
            ))
            .textFieldStyle(RoundedBorderTextFieldStyle())
            .padding()

            // Visualizza la vista specifica del cue
            cue.cueView()
        }
    }
}
