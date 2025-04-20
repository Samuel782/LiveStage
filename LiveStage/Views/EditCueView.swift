//
//  EditCueView.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

import SwiftUI

struct EditCueView: View{
    
    @ObservedObject var cue: Cue

    var body: some View{
        
        VStack{
            TextField("Title:", text: $cue.title)
                .textFieldStyle(RoundedBorderTextFieldStyle())
                .padding()
             
            TextField("Notes:", text: Binding(
                get: { cue.notes ?? "" },
                set: { cue.notes = $0 }
            ))
            .textFieldStyle(RoundedBorderTextFieldStyle())
            .padding()
            
            
            cue.cueView()
        }
    }
    
}
