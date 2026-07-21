func process(flag: Bool): Unit {
    try {
        if (flag) {
            return
        }
        work()
    } finally {
        cleanup()
    }
    after()
}
