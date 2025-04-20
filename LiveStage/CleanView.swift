struct CleanView: View {
    @EnvironmentObject var playerModel: PlayerModel
    
    var body: some View {
        VideoPlayer(player: playerModel.player)
            .edgesIgnoringSafeArea(.all)
    }
}
