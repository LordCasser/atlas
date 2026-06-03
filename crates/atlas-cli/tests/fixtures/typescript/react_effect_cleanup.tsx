function Timer() {
    useEffect(() => {
        const id = setInterval(() => {}, 1000);
        return () => clearInterval(id);
    });
    return null;
}
