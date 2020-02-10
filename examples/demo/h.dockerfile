FROM python:3

WORKDIR /usr/src/app

#COPY requirements.txt ./
RUN pip install --no-cache-dir aiomsg

COPY . .

CMD [ "python", "./your-daemon-or-script.py" ]
