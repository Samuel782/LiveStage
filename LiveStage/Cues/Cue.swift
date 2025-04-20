//
//  Cue.swift
//  LiveStage
//
//  Created by Samuel Luggeri on 20/04/25.
//

import Foundation

class Cue: Identifiable {
    
    var id = UUID()
    var title: String
    var notes: String?
    // var startTimecode: Timecode
    
    init(title: String, notes: String? = nil) {
        self.title = title
        self.notes = notes
    }

    func setTitle(title: String){
        self.title = title
    }
}
