using System;
using System.IO;

class ResourceDemo
{
    void ReadFile()
    {
        using (var stream = new FileStream("data.txt", FileMode.Open))
        {
            byte[] buffer = new byte[1024];
            stream.Read(buffer, 0, buffer.Length);
        }
    }
}
